//! Đo độ trễ K04 -> NewOrderSingle đầu–cuối trên loopback, dùng đúng Session/Trigger/sender
//! của racer. Sở giả ghi K04 vào socket (t0) rồi spin đọc tới khi nhận đủ các lệnh D.
//!
//!   cargo run --release --example latency -- [--rounds 2000] [--orders 1] [--batch 32] [--no-busy-poll]

use std::io::{BufReader, ErrorKind, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use order_race::clock;
use order_race::config::SessionCfg;
use order_race::fix::{self, msg, tag, FrameReader, Header, OrderTemplate};
use order_race::journal::Journal;
use order_race::market::Market;
use order_race::order::{OrdKind, Order, Side};
use order_race::sender::{self, FireWindow};
use order_race::session::{Common, Handler, Session};
use order_race::trigger::Trigger;

struct Arm(Arc<Trigger>);

impl Handler for Arm {
    fn on_app_message(&self, s: &Session, mt: &[u8], m: &[u8]) {
        if mt == msg::TRADING_SESSION_STATUS {
            let market = fix::get(m, tag::ON_BEHALF_OF_COMP_ID).unwrap_or(b"");
            let evt = fix::get(m, tag::BOARD_EVT_ID).unwrap_or(b"");
            self.0.on_k04(market, evt, s.name());
        }
    }
}

fn order(i: usize) -> Order {
    Order {
        line: i,
        received_at: None,
        account: "011C000001".into(),
        market: Market::Sto,
        board: "G1".into(),
        symbol: "VN000000HCM0".into(),
        side: Side::Buy,
        kind: OrdKind::Lo,
        qty: 100,
        price: 24_200,
        account_type: "1".into(),
        cash_margin: "1".into(),
        invest_code: "1".into(),
        forn_invest_type_code: "00".into(),
        cl_ord_id: format!("L{i:08}"),
    }
}

fn now_ts() -> clock::Timestamp {
    clock::utc_timestamp(clock::now_nanos())
}

/// Spin đọc non-blocking tới khi nhận đủ `n` lệnh D; trả thời điểm nhận đủ lệnh đầu và lệnh cuối.
fn spin_read_orders(sock: &mut TcpStream, frames: &mut FrameReader, n: usize) -> (Instant, Instant) {
    let (mut got, mut first) = (0, None);
    loop {
        while let Some(m) = frames.next_frame().unwrap() {
            assert_eq!(fix::msg_type(m), b"D");
            got += 1;
            let now = Instant::now();
            first.get_or_insert(now);
            if got == n {
                return (first.unwrap(), now);
            }
        }
        match frames.fill(sock) {
            Ok(0) => panic!("racer đóng kết nối"),
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::WouldBlock => std::hint::spin_loop(),
            Err(e) => panic!("đọc lỗi: {e}"),
        }
    }
}

fn pct(v: &mut [Duration], p: f64) -> Duration {
    v.sort();
    v[((v.len() - 1) as f64 * p).round() as usize]
}

fn main() {
    let (mut rounds, mut per_round, mut batch, mut busy_poll) = (2000usize, 1usize, 32u32, true);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut num = || args.next().and_then(|v| v.parse::<usize>().ok()).expect("thiếu số sau tham số");
        match a.as_str() {
            "--rounds" => rounds = num(),
            "--orders" => per_round = num(),
            "--batch" => batch = num() as u32,
            "--no-busy-poll" => busy_poll = false,
            other => panic!("tham số lạ {other}"),
        }
    }

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let exch_hdr = Header { begin_string: "FIX.4.4".into(), sender: "VOMGW".into(), target: "HOSE02".into() };
    let acceptor = thread::spawn(move || {
        let (sock, _) = listener.accept().unwrap();
        sock.set_nodelay(true).unwrap();
        let mut r = BufReader::new(sock.try_clone().unwrap());
        fix::read_message(&mut r, &mut Vec::new()).unwrap(); // logon của racer
        sock
    });

    let dir = std::env::temp_dir().join("racer_latency");
    std::fs::create_dir_all(&dir).unwrap();
    let common = Arc::new(Common {
        begin_string: "FIX.4.4".into(),
        heartbeat: Duration::from_secs(30),
        journal: Arc::new(Journal::open(&dir.join("fix.log"), 7 * 3600).unwrap()),
        journal_dir: dir.clone(),
        day: "bench".into(),
    });
    let trigger = Arc::new(Trigger::new([vec!["AA1".into()], vec![], vec![]], 0));
    let cfg = SessionCfg {
        name: "bench".into(),
        addr: addr.to_string(),
        sender: "HOSE02".into(),
        target: "VOMGW".into(),
        markets: vec![Market::Sto],
        rate: 1e9,
        burst: 1_000_000,
        batch,
        busy_poll,
        reset_seq: true,
    };
    let connecting = {
        let (common, trigger) = (common.clone(), trigger.clone());
        thread::spawn(move || Session::connect(0, cfg, common, Arc::new(Arm(trigger)), Instant::now() + Duration::from_secs(5)))
    };
    let mut sock = acceptor.join().unwrap();
    let mut logon = Vec::new();
    let mut body = Vec::new();
    fix::put_field_u64(&mut body, tag::ENCRYPT_METHOD, 0);
    fix::put_field_u64(&mut body, tag::HEART_BT_INT, 30);
    exch_hdr.encode(&mut logon, msg::LOGON, 1, &now_ts(), &body);
    sock.write_all(&logon).unwrap();
    let session = connecting.join().unwrap().unwrap();
    session.set_busy_poll(busy_poll);
    sock.set_nonblocking(true).unwrap();
    thread::sleep(Duration::from_millis(200)); // thread đọc kịp đổi chế độ

    let mut k04_body = Vec::new();
    fix::put_field(&mut k04_body, tag::ON_BEHALF_OF_COMP_ID, b"STO");
    fix::put_field(&mut k04_body, 20003, b"STO");
    fix::put_field(&mut k04_body, tag::BOARD_EVT_ID, b"AA1");

    let (mut to_arm, mut first_d, mut last_d) = (Vec::new(), Vec::new(), Vec::new());
    let mut frames = FrameReader::new(1 << 16);
    for i in 0..rounds {
        trigger.reset();
        let templates: Vec<_> = (0..per_round).map(|k| OrderTemplate::new(k, &order(i * per_round + k), Some("HSC"), "20261025")).collect();
        let (s, trg) = (session.clone(), trigger.clone());
        let fire = thread::spawn(move || {
            let w = FireWindow { spin_from: Instant::now(), give_up_at: Instant::now() + Duration::from_secs(5) };
            sender::run(&s, templates, &trg, w)
        });
        thread::sleep(Duration::from_millis(2)); // để thread gửi vào vòng spin

        let mut k04 = Vec::new();
        exch_hdr.encode(&mut k04, msg::TRADING_SESSION_STATUS, i as u64 + 2, &now_ts(), &k04_body);
        let t0 = Instant::now();
        sock.write_all(&k04).unwrap();
        let (t_first, t_last) = spin_read_orders(&mut sock, &mut frames, per_round);

        let out = fire.join().unwrap();
        assert_eq!(out.sent.len(), per_round);
        let armed = trigger.info(Market::Sto).unwrap().at;
        if i >= rounds / 20 {
            // bỏ 5% vòng đầu (khởi động)
            to_arm.push(armed.saturating_duration_since(t0));
            first_d.push(t_first - t0);
            last_d.push(t_last - t0);
        }
    }
    session.logout(Duration::from_millis(200));

    println!(
        "{} vòng, {} lệnh/vòng, batch {}, busy-poll {}",
        first_d.len(),
        per_round,
        batch,
        if busy_poll { "bật" } else { "tắt" }
    );
    println!("{:<30} {:>9} {:>9} {:>9} {:>9}", "", "p50", "p90", "p99", "max");
    let mut rows = vec![("K04 ghi -> bật cờ", &mut to_arm), ("K04 ghi -> sở nhận đủ D đầu", &mut first_d)];
    if per_round > 1 {
        rows.push(("K04 ghi -> sở nhận đủ D cuối", &mut last_d));
    }
    for (name, v) in rows {
        let max = *v.iter().max().unwrap();
        println!(
            "{name:<30} {:>9} {:>9} {:>9} {:>9}",
            clock::fmt_dur(pct(v, 0.5)),
            clock::fmt_dur(pct(v, 0.9)),
            clock::fmt_dur(pct(v, 0.99)),
            clock::fmt_dur(max)
        );
    }
}
