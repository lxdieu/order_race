//! Một FIX session phía racer (initiator). Mỗi session có thread đọc riêng (trả lời
//! TestRequest/ResendRequest, chuyển message nghiệp vụ cho Handler) và thread heartbeat.
//! Mọi lần ghi socket đi qua một Mutex giữ seq và buffer, nên seq luôn đúng thứ tự trên wire.
//!
//! Busy-poll: bình thường thread đọc chặn trong recv() và chờ kernel đánh thức khi có dữ liệu
//! (cỡ chục µs). Trong cửa sổ đua lệnh, thread đọc chuyển socket sang non-blocking và spin, nên
//! K04 được xử lý ngay khi tới. Chế độ non-blocking dùng chung cho cả socket, nên mọi lần ghi
//! đều phải chịu được WouldBlock (write_all_spin).

use std::fs;
use std::io::{self, ErrorKind, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::clock::{self, Timestamp};
use crate::config::SessionCfg;
use crate::fix::{self, msg, tag, FrameReader, Header};
use crate::journal::Journal;

pub trait Handler: Send + Sync {
    /// Gọi trên thread đọc của session, cho mọi message không phải message quản trị session.
    fn on_app_message(&self, session: &Session, msg_type: &[u8], msg: &[u8]);
}

/// Phần dùng chung cho mọi session.
pub struct Common {
    pub begin_string: String,
    pub heartbeat: Duration,
    pub journal: Arc<Journal>,
    pub journal_dir: PathBuf,
    pub day: String,
}

struct Out {
    stream: TcpStream,
    seq: u64,
    buf: Vec<u8>,
    last_send: Instant,
}

pub struct Session {
    pub idx: usize,
    pub cfg: SessionCfg,
    pub hdr: Header,
    common: Arc<Common>,
    out: Mutex<Out>,
    in_seq: AtomicU64,
    alive: AtomicBool,
    closing: AtomicBool,
    busy_poll: AtomicBool,
    done: Mutex<Receiver<()>>,
}

/// Chu kỳ thread đọc (chế độ chặn) thức dậy để xem có phải chuyển sang busy-poll không.
const BLOCKING_READ_TIMEOUT: Duration = Duration::from_millis(100);

impl Session {
    /// Kết nối, logon và chạy thread đọc + heartbeat. `deadline` giới hạn cả kết nối lẫn chờ logon.
    pub fn connect(idx: usize, cfg: SessionCfg, common: Arc<Common>, handler: Arc<dyn Handler>, deadline: Instant) -> Result<Arc<Session>, String> {
        let name = cfg.name.clone();
        let err = |what: &str, e: &dyn std::fmt::Display| format!("{name}: {what}: {e}");
        let seq_path = common.journal_dir.join(format!("{}_{}.seq", common.day, cfg.name));
        let (out_seq, in_seq) = if cfg.reset_seq { (1, 0) } else { load_seq(&seq_path).map_err(|e| err("đọc seq", &e))? };
        let left = || deadline.saturating_duration_since(Instant::now()).max(Duration::from_millis(100));

        let addr = cfg.addr.to_socket_addrs().map_err(|e| err("địa chỉ", &e))?.next().ok_or_else(|| err("địa chỉ", &"không phân giải được"))?;
        let stream = TcpStream::connect_timeout(&addr, left()).map_err(|e| err(&format!("kết nối {}", cfg.addr), &e))?;
        stream.set_nodelay(true).map_err(|e| err("TCP_NODELAY", &e))?;
        let mut rstream = stream.try_clone().map_err(|e| err("clone socket", &e))?;

        let hdr = Header { begin_string: common.begin_string.clone(), sender: cfg.sender.clone(), target: cfg.target.clone() };
        let (done_tx, done_rx) = mpsc::channel();
        let session = Arc::new(Session {
            idx,
            hdr,
            common,
            out: Mutex::new(Out { stream, seq: out_seq, buf: Vec::with_capacity(1 << 14), last_send: Instant::now() }),
            in_seq: AtomicU64::new(in_seq),
            alive: AtomicBool::new(true),
            closing: AtomicBool::new(false),
            busy_poll: AtomicBool::new(false),
            done: Mutex::new(done_rx),
            cfg,
        });

        let mut body = Vec::new();
        fix::put_field_u64(&mut body, tag::ENCRYPT_METHOD, 0);
        fix::put_field_u64(&mut body, tag::HEART_BT_INT, session.common.heartbeat.as_secs());
        if session.cfg.reset_seq {
            fix::put_field(&mut body, tag::RESET_SEQ_NUM_FLAG, b"Y");
        }
        session.send_admin(msg::LOGON, &body).map_err(|e| err("gửi logon", &e))?;

        let mut frames = FrameReader::new(1 << 16);
        let logon = rstream.set_read_timeout(Some(left())).and_then(|_| loop {
            if let Some(m) = frames.next_frame()? {
                break Ok(m.to_vec());
            }
            if frames.fill(&mut rstream)? == 0 {
                break Err(ErrorKind::UnexpectedEof.into());
            }
        });
        let logon = match logon {
            Ok(m) => m,
            Err(e) => {
                session.kill();
                return Err(err("chờ logon", &e));
            }
        };
        session.journal_in(&logon);
        session.track_in_seq(&logon);
        if fix::msg_type(&logon) != msg::LOGON {
            session.kill();
            return Err(format!("{name}: logon bị từ chối (35={}): {}", fix::get_str(&logon, tag::MSG_TYPE), fix::get_str(&logon, tag::TEXT)));
        }

        let s = Arc::clone(&session);
        thread::Builder::new()
            .name(format!("read-{name}"))
            .spawn(move || {
                s.read_loop(rstream, frames, handler);
                s.alive.store(false, Ordering::Release);
                let _ = done_tx.send(());
            })
            .map_err(|e| err("tạo thread đọc", &e))?;
        let s = Arc::clone(&session);
        thread::Builder::new().name(format!("hb-{name}")).spawn(move || s.heartbeat_loop()).map_err(|e| err("tạo thread heartbeat", &e))?;
        Ok(session)
    }

    pub fn name(&self) -> &str {
        &self.cfg.name
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    /// Bật/tắt busy-poll của thread đọc (có hiệu lực trong tối đa 100ms).
    pub fn set_busy_poll(&self, on: bool) {
        self.busy_poll.store(on, Ordering::Release);
    }

    /// Gửi một lô lệnh trong một lần write. `fill` ghi các message (seq liên tiếp từ `first_seq`,
    /// cùng SendingTime) vào buffer và trả số message đã ghi. Đường nóng: không cấp phát, không log.
    /// Trả (seq đầu, số message).
    #[inline]
    pub fn send_batch(&self, fill: impl FnOnce(&mut Vec<u8>, &Header, u64, &Timestamp) -> u64) -> io::Result<(u64, u64)> {
        let mut g = self.out.lock().unwrap();
        let out = &mut *g;
        let first = out.seq;
        let ts = clock::utc_timestamp(clock::now_nanos());
        out.buf.clear();
        let n = fill(&mut out.buf, &self.hdr, first, &ts);
        if n > 0 {
            write_all_spin(&mut out.stream, &out.buf)?;
            out.seq += n;
            out.last_send = Instant::now();
        }
        Ok((first, n))
    }

    fn send_admin(&self, msg_type: &[u8], body: &[u8]) -> io::Result<()> {
        let mut g = self.out.lock().unwrap();
        let out = &mut *g;
        out.buf.clear();
        self.hdr.encode(&mut out.buf, msg_type, out.seq, &clock::utc_timestamp(clock::now_nanos()), body);
        write_all_spin(&mut out.stream, &out.buf)?;
        out.seq += 1;
        out.last_send = Instant::now();
        self.common.journal.line(&format!("{} OUT {}", self.cfg.name, fix::pretty(&out.buf)));
        Ok(())
    }

    /// Trả lời ResendRequest bằng SequenceReset-GapFill tới seq kế tiếp. Không gửi lại lệnh cũ:
    /// gửi lại lệnh (PossDup) có rủi ro thành lệnh trùng; lệnh thiếu phản hồi sẽ hiện ra lúc đối soát.
    fn gap_fill(&self, begin: u64) -> io::Result<()> {
        let mut g = self.out.lock().unwrap();
        let out = &mut *g;
        let ts = clock::utc_timestamp(clock::now_nanos());
        let mut body = Vec::with_capacity(64);
        fix::put_field(&mut body, tag::POSS_DUP_FLAG, b"Y");
        fix::put_field(&mut body, tag::ORIG_SENDING_TIME, &ts);
        fix::put_field(&mut body, tag::GAP_FILL_FLAG, b"Y");
        fix::put_field_u64(&mut body, tag::NEW_SEQ_NO, out.seq);
        out.buf.clear();
        self.hdr.encode(&mut out.buf, msg::SEQUENCE_RESET, begin, &ts, &body);
        write_all_spin(&mut out.stream, &out.buf)?;
        self.common.journal.line(&format!("{} OUT {}", self.cfg.name, fix::pretty(&out.buf)));
        Ok(())
    }

    fn read_loop(&self, mut stream: TcpStream, mut frames: FrameReader, handler: Arc<dyn Handler>) {
        let mut busy = false;
        if let Err(e) = stream.set_read_timeout(Some(BLOCKING_READ_TIMEOUT)) {
            eprintln!("{}: đặt read timeout: {e}", self.cfg.name);
        }
        loop {
            // xử lý hết message đã có trong buffer
            loop {
                let m = match frames.next_frame() {
                    Ok(Some(m)) => m,
                    Ok(None) => break,
                    Err(e) => return self.lost(&e),
                };
                if !self.on_frame(m, &*handler) {
                    return;
                }
            }
            // đọc thêm: spin non-blocking trong cửa sổ đua, ngoài ra chặn có timeout
            let want = self.busy_poll.load(Ordering::Acquire);
            if want != busy {
                busy = want;
                if let Err(e) = stream.set_nonblocking(busy) {
                    eprintln!("{}: đổi chế độ socket: {e}", self.cfg.name);
                }
            }
            match frames.fill(&mut stream) {
                Ok(0) => return self.lost(&io::Error::from(ErrorKind::UnexpectedEof)),
                Ok(_) => {}
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted) => {
                    if busy {
                        std::hint::spin_loop();
                    }
                }
                Err(e) => return self.lost(&e),
            }
        }
    }

    /// Xử lý một message nhận được. Trả false khi session kết thúc (Logout).
    fn on_frame(&self, m: &[u8], handler: &dyn Handler) -> bool {
        let mt = fix::msg_type(m);
        match mt {
            msg::HEARTBEAT => {}
            msg::TEST_REQUEST => {
                let mut body = Vec::new();
                fix::put_field(&mut body, tag::TEST_REQ_ID, fix::get(m, tag::TEST_REQ_ID).unwrap_or(b""));
                let _ = self.send_admin(msg::HEARTBEAT, &body);
            }
            msg::RESEND_REQUEST => {
                let begin = fix::get_u64(m, tag::BEGIN_SEQ_NO).unwrap_or(1);
                self.common.journal.line(&format!("{} CẢNH BÁO: sở xin gửi lại từ seq {begin}, trả GapFill", self.cfg.name));
                let _ = self.gap_fill(begin);
            }
            msg::SEQUENCE_RESET => {
                if let Some(n) = fix::get_u64(m, tag::NEW_SEQ_NO) {
                    self.in_seq.store(n.saturating_sub(1), Ordering::Relaxed);
                }
            }
            msg::LOGOUT => {
                self.journal_in(m);
                if !self.closing.swap(true, Ordering::AcqRel) {
                    eprintln!("{}: sở gửi Logout: {}", self.cfg.name, fix::get_str(m, tag::TEXT));
                    let _ = self.send_admin(msg::LOGOUT, b"");
                }
                return false;
            }
            // xử lý nghiệp vụ trước (K04 kích hoạt bắn lệnh), ghi log sau
            _ => handler.on_app_message(self, mt, m),
        }
        self.journal_in(m);
        if mt != msg::SEQUENCE_RESET {
            self.track_in_seq(m);
        }
        true
    }

    fn lost(&self, e: &io::Error) {
        if !self.closing.load(Ordering::Acquire) {
            let text = format!("{}: mất kết nối: {e}", self.cfg.name);
            self.common.journal.line(&text);
            eprintln!("{text}");
        }
    }

    fn heartbeat_loop(&self) {
        let hb = self.common.heartbeat;
        while self.is_alive() {
            thread::sleep(Duration::from_millis(500));
            let idle = self.out.lock().unwrap().last_send.elapsed();
            if idle >= hb && self.is_alive() && self.send_admin(msg::HEARTBEAT, b"").is_err() {
                return;
            }
        }
    }

    fn track_in_seq(&self, m: &[u8]) {
        let Some(seq) = fix::get_u64(m, tag::MSG_SEQ_NUM) else { return };
        let expected = self.in_seq.load(Ordering::Relaxed) + 1;
        if seq > expected && fix::get(m, tag::POSS_DUP_FLAG) != Some(b"Y") {
            self.common.journal.line(&format!("{} CẢNH BÁO: gap seq chiều vào, chờ {expected} nhận {seq}", self.cfg.name));
        }
        self.in_seq.fetch_max(seq, Ordering::Relaxed);
    }

    fn journal_in(&self, m: &[u8]) {
        self.common.journal.line(&format!("{} IN  {}", self.cfg.name, fix::pretty(m)));
    }

    /// Logout lịch sự, chờ sở trả Logout tối đa `timeout`, rồi đóng socket và lưu seq.
    pub fn logout(&self, timeout: Duration) {
        self.set_busy_poll(false);
        if self.is_alive() && !self.closing.swap(true, Ordering::AcqRel) {
            let _ = self.send_admin(msg::LOGOUT, b"");
        }
        let _ = self.done.lock().unwrap().recv_timeout(timeout);
        self.kill();
        if !self.cfg.reset_seq {
            let out = self.out.lock().unwrap().seq;
            let path = self.common.journal_dir.join(format!("{}_{}.seq", self.common.day, self.cfg.name));
            if let Err(e) = fs::write(&path, format!("{out} {}\n", self.in_seq.load(Ordering::Relaxed))) {
                eprintln!("{}: không lưu được seq vào {}: {e}", self.cfg.name, path.display());
            }
        }
    }

    fn kill(&self) {
        self.closing.store(true, Ordering::Release);
        self.alive.store(false, Ordering::Release);
        let _ = self.out.lock().unwrap().stream.shutdown(Shutdown::Both);
    }
}

/// write_all chịu được socket non-blocking (thread đọc đang busy-poll): WouldBlock thì spin ghi tiếp.
fn write_all_spin(stream: &mut TcpStream, mut buf: &[u8]) -> io::Result<()> {
    while !buf.is_empty() {
        match stream.write(buf) {
            Ok(0) => return Err(ErrorKind::WriteZero.into()),
            Ok(n) => buf = &buf[n..],
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => std::hint::spin_loop(),
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// File seq trong ngày: "<seq gửi kế tiếp> <seq nhận cuối>". Chưa có file thì bắt đầu từ 1.
fn load_seq(path: &Path) -> io::Result<(u64, u64)> {
    match fs::read_to_string(path) {
        Ok(text) => {
            let mut it = text.split_whitespace().map(str::parse::<u64>);
            match (it.next(), it.next()) {
                (Some(Ok(out)), Some(Ok(inn))) if out > 0 => Ok((out, inn)),
                _ => Err(io::Error::new(ErrorKind::InvalidData, format!("file seq hỏng: {}", path.display()))),
            }
        }
        Err(e) if e.kind() == ErrorKind::NotFound => Ok((1, 0)),
        Err(e) => Err(e),
    }
}
