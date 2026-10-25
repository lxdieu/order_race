//! racer: đua lệnh đầu ngày. Nạp lệnh, kiểm tra và xếp thứ tự trước giờ mở cửa, logon sẵn các
//! session, rồi bắn lệnh ngay khi nhận K04 mở phiên của từng thị trường.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use std::{env, fs};

use order_race::config::{self, Config};
use order_race::fix::{self, msg, tag, OrderTemplate};
use order_race::journal::{self, Journal};
use order_race::market::Market;
use order_race::session::{Common, Handler, Session};
use order_race::track::{State, Tracker};
use order_race::trigger::{K04Outcome, Source, Trigger};
use order_race::{clock, order, plan, sender};

const USAGE: &str = "\
racer [--env FILE] [--dry-run]
  --env FILE   file KEY=VALUE nạp vào môi trường (mặc định .env; biến đã có sẵn được giữ)
  --dry-run    chỉ kiểm tra lệnh và in kế hoạch gửi, không kết nối";

fn main() -> ExitCode {
    let mut env_file = PathBuf::from(".env");
    let mut dry_run = false;
    let mut args = env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--env" => match args.next() {
                Some(p) => env_file = p.into(),
                None => return usage_error("--env cần đường dẫn"),
            },
            "--dry-run" => dry_run = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other => return usage_error(&format!("tham số lạ {other:?}")),
        }
    }
    match run(&env_file, dry_run) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("lỗi: {e}");
            ExitCode::from(2)
        }
    }
}

fn usage_error(msg: &str) -> ExitCode {
    eprintln!("{msg}\n{USAGE}");
    ExitCode::from(2)
}

/// Log ra stdout kèm giờ địa phương (µs).
macro_rules! say {
    ($cfg:expr, $($arg:tt)*) => {
        println!("{} {}", clock::local_time_us(clock::now_nanos(), $cfg.utc_offset), format!($($arg)*))
    };
}

/// Trả Ok(true) khi mọi lệnh được sở nhận; Ok(false) khi có lệnh bị loại, từ chối hoặc không phản hồi.
fn run(env_file: &Path, dry_run: bool) -> Result<bool, String> {
    match config::load_dotenv(env_file) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("đọc {}: {e}", env_file.display())),
    }
    let cfg = config::load(&|k| env::var(k).ok(), clock::now_nanos())?;
    if !dry_run && cfg.open_at + (cfg.give_up_after.as_nanos() as i64) < clock::now_nanos() {
        return Err(format!(
            "giờ mở {} đã qua: HH:MM:SS tính theo ngày hôm nay, chạy racer buổi sáng trước giờ mở hoặc ghi RACE_OPEN_TIME dạng RFC 3339 có ngày",
            clock::local_time_us(cfg.open_at, cfg.utc_offset)
        ));
    }
    let day = clock::local_yyyymmdd(cfg.open_at, cfg.utc_offset);
    say!(cfg, "giờ mở phiên dự kiến {} (ngày giao dịch {day})", clock::local_time_us(cfg.open_at, cfg.utc_offset));

    // 1. nạp, kiểm tra, xếp thứ tự
    let text = fs::read_to_string(&cfg.orders_file).map_err(|e| format!("đọc {}: {e}", cfg.orders_file.display()))?;
    let (orders, mut problems) = order::load_csv(&text);
    let total_in = orders.len() + problems.len();
    let orders: Vec<_> = orders
        .into_iter()
        .filter(|o| match order::validate(o, cfg.check_tick) {
            Ok(()) => true,
            Err(e) => {
                problems.push(format!("dòng {}: {e}", o.line));
                false
            }
        })
        .collect();
    let mut plan = plan::build(orders, &cfg.sessions);
    for o in &plan.unroutable {
        problems.push(format!("dòng {}: không có session nào gửi lệnh {}", o.line, o.market.exchange()));
    }
    plan.assign_cl_ord_ids(&cfg.clordid_prefix, &day);
    for p in &problems {
        say!(cfg, "LOẠI {p}");
    }
    say!(cfg, "{} lệnh trong file, {} hợp lệ sẽ gửi, {} bị loại", total_in, plan.sorted.len(), problems.len());
    for (i, s) in cfg.sessions.iter().enumerate() {
        let n = plan.per_session[i].len();
        if s.markets.is_empty() {
            say!(cfg, "  session {:<10} chỉ nghe tin", s.name);
        } else {
            let markets: Vec<_> = s.markets.iter().map(|m| m.id()).collect();
            let est = Duration::from_secs_f64(n.saturating_sub(s.burst as usize) as f64 / s.rate);
            say!(cfg, "  session {:<10} {:>6} lệnh {:?}, {} lệnh/s, xả hết ~{}", s.name, n, markets, s.rate, clock::fmt_dur(est));
        }
    }
    for m in Market::ALL {
        let n = plan.sorted.iter().filter(|o| o.market == m).count();
        if n > 0 {
            say!(cfg, "  {} {:>6} lệnh, bắn khi nhận K04 {:?}", m.id(), n, cfg.triggers[m.index()]);
        }
    }

    fs::create_dir_all(&cfg.journal_dir).map_err(|e| format!("tạo {}: {e}", cfg.journal_dir.display()))?;
    let plan_path = cfg.journal_dir.join(format!("{day}_plan.csv"));
    journal::write_synced(&plan_path, plan_csv(&plan, &cfg).as_bytes()).map_err(|e| format!("ghi {}: {e}", plan_path.display()))?;
    say!(cfg, "kế hoạch gửi: {}", plan_path.display());
    if dry_run {
        return Ok(problems.is_empty());
    }
    if plan.sorted.is_empty() {
        return Err("không có lệnh nào để gửi".into());
    }

    // 2. dựng sẵn message
    let mut templates: Vec<Vec<OrderTemplate>> = plan
        .per_session
        .iter()
        .map(|ranks| ranks.iter().map(|&r| OrderTemplate::new(r, &plan.sorted[r], cfg.sender_sub_id.as_deref(), &day)).collect())
        .collect();

    let journal = Arc::new(Journal::open(&cfg.journal_dir.join(format!("{day}_fix.log")), cfg.utc_offset).map_err(|e| format!("mở journal: {e}"))?);
    let tracker = Arc::new(Tracker::new(plan.sorted.iter().map(|o| o.cl_ord_id.clone()).collect(), plan.session_of.clone()));
    let earliest = cfg.open_at - cfg.k04_earliest.as_nanos() as i64;
    let trigger = Arc::new(Trigger::new(cfg.triggers.clone(), earliest));
    let router: Arc<dyn Handler> = Arc::new(Router { trigger: trigger.clone(), tracker: tracker.clone(), journal: journal.clone(), utc_offset: cfg.utc_offset });
    let common = Arc::new(Common {
        begin_string: cfg.begin_string.clone(),
        heartbeat: cfg.heartbeat,
        journal: journal.clone(),
        journal_dir: cfg.journal_dir.clone(),
        day: day.clone(),
    });

    // 3. chờ tới giờ kết nối, logon mọi session cần dùng
    let connect_at = cfg.open_at - cfg.connect_lead.as_nanos() as i64;
    if connect_at > clock::now_nanos() {
        say!(cfg, "chờ tới {} để kết nối", clock::local_time_us(connect_at, cfg.utc_offset));
        clock::sleep_until(connect_at);
    }
    let needed: Vec<usize> = (0..cfg.sessions.len()).filter(|&i| cfg.sessions[i].markets.is_empty() || !plan.per_session[i].is_empty()).collect();
    let sessions = connect_all(&cfg, &needed, &common, &router);

    // 4. thread gửi chờ cờ; luồng dự phòng theo đồng hồ (nếu bật)
    let window = || sender::FireWindow {
        spin_from: clock::instant_at(cfg.open_at - cfg.spin_lead.as_nanos() as i64),
        give_up_at: clock::instant_at(cfg.open_at + cfg.give_up_after.as_nanos() as i64),
    };
    let mut handles = Vec::new();
    let mut unsent_no_session = 0;
    for (i, slot) in sessions.iter().enumerate() {
        let orders = std::mem::take(&mut templates[i]);
        if orders.is_empty() {
            continue;
        }
        let Some(s) = slot.clone() else {
            unsent_no_session += orders.len();
            say!(cfg, "{}: không logon được, {} lệnh sẽ không gửi", cfg.sessions[i].name, orders.len());
            continue;
        };
        let (trigger, w) = (trigger.clone(), window());
        let h = thread::Builder::new()
            .name(format!("send-{}", s.name()))
            .spawn(move || sender::run(&s, orders, &trigger, w))
            .map_err(|e| format!("tạo thread gửi: {e}"))?;
        handles.push((i, h));
    }
    let active_markets: Vec<Market> = Market::ALL.into_iter().filter(|m| plan.sorted.iter().any(|o| o.market == *m)).collect();
    // busy-poll từ đầu cửa sổ spin tới khi mọi thị trường đã mở (sau đó không còn K04 để chờ)
    let polled: Vec<Arc<Session>> = sessions.iter().flatten().filter(|s| s.cfg.busy_poll).cloned().collect();
    if !polled.is_empty() {
        let spin_at = cfg.open_at - cfg.spin_lead.as_nanos() as i64;
        let give_up = cfg.open_at + cfg.give_up_after.as_nanos() as i64;
        let all = active_markets.iter().fold(0, |mask, m| mask | m.bit());
        let trigger = trigger.clone();
        thread::Builder::new()
            .name("busy-poll".into())
            .spawn(move || {
                clock::sleep_until(spin_at);
                polled.iter().for_each(|s| s.set_busy_poll(true));
                while trigger.armed_bits() & all != all && clock::now_nanos() < give_up {
                    thread::sleep(Duration::from_millis(1));
                }
                polled.iter().for_each(|s| s.set_busy_poll(false));
            })
            .map_err(|e| format!("tạo thread busy-poll: {e}"))?;
    }
    if let Some(delay) = cfg.fallback_delay {
        let fire_at = cfg.open_at + delay.as_nanos() as i64;
        let (trigger, markets, offset) = (trigger.clone(), active_markets.clone(), cfg.utc_offset);
        thread::Builder::new()
            .name("fallback".into())
            .spawn(move || {
                clock::sleep_until(fire_at);
                for m in markets {
                    if trigger.arm(m, Source::Clock) {
                        println!("{} {} chưa có K04 mở phiên, bắn theo đồng hồ (dự phòng)", clock::local_time_us(clock::now_nanos(), offset), m.id());
                    }
                }
            })
            .map_err(|e| format!("tạo thread dự phòng: {e}"))?;
    }
    say!(cfg, "đã sẵn sàng, chờ K04 mở phiên{}", match cfg.fallback_delay {
        Some(d) => format!(" (dự phòng theo đồng hồ sau giờ mở {})", clock::fmt_dur(d)),
        None => String::new(),
    });

    // 5. chờ gửi xong, chờ phản hồi
    let mut unsent = unsent_no_session;
    for (i, h) in handles {
        let out = h.join().map_err(|_| "thread gửi panic".to_string())?;
        if let Some(e) = &out.error {
            say!(cfg, "{}: dừng gửi: {e} ({} lệnh chưa gửi)", cfg.sessions[i].name, out.unsent);
        }
        unsent += out.unsent;
        tracker.record_sent(&out.sent);
    }
    say!(cfg, "đã gửi xong, chờ phản hồi của sở tối đa {}", clock::fmt_dur(cfg.ack_timeout));
    if !tracker.wait(cfg.ack_timeout) {
        say!(cfg, "hết giờ chờ phản hồi");
    }

    let ok = summarize(&cfg, &plan, &tracker, &trigger, &active_markets, unsent) && problems.is_empty();
    for s in sessions.iter().flatten() {
        s.logout(Duration::from_secs(2));
    }
    journal.close().map_err(|e| format!("đóng journal: {e}"))?;
    Ok(ok)
}

/// Logon song song; session lỗi thì thử lại mỗi 2s cho tới 1s trước giờ mở.
fn connect_all(cfg: &Config, needed: &[usize], common: &Arc<Common>, router: &Arc<dyn Handler>) -> Vec<Option<Arc<Session>>> {
    let last_try = clock::instant_at(cfg.open_at - 1_000_000_000);
    let results: Vec<(usize, Option<Arc<Session>>)> = thread::scope(|scope| {
        let workers: Vec<_> = needed
            .iter()
            .map(|&i| {
                scope.spawn(move || loop {
                    let deadline = (Instant::now() + Duration::from_secs(5)).min(last_try.max(Instant::now() + Duration::from_millis(500)));
                    match Session::connect(i, cfg.sessions[i].clone(), common.clone(), router.clone(), deadline) {
                        Ok(s) => {
                            say!(cfg, "{}: đã logon {} -> {} ({})", s.name(), s.hdr.sender, s.hdr.target, cfg.sessions[i].addr);
                            return (i, Some(s));
                        }
                        Err(e) => {
                            say!(cfg, "{e}");
                            if Instant::now() + Duration::from_secs(2) >= last_try {
                                return (i, None);
                            }
                            thread::sleep(Duration::from_secs(2));
                        }
                    }
                })
            })
            .collect();
        workers.into_iter().map(|w| w.join().unwrap()).collect()
    });
    let mut out = vec![None; cfg.sessions.len()];
    for (i, s) in results {
        out[i] = s;
    }
    out
}

/// Chuyển message nghiệp vụ từ thread đọc: K04 tới Trigger, phản hồi lệnh tới Tracker.
struct Router {
    trigger: Arc<Trigger>,
    tracker: Arc<Tracker>,
    journal: Arc<Journal>,
    utc_offset: i32,
}

impl Handler for Router {
    fn on_app_message(&self, s: &Session, msg_type: &[u8], m: &[u8]) {
        match msg_type {
            msg::TRADING_SESSION_STATUS => {
                let market = fix::get(m, tag::ON_BEHALF_OF_COMP_ID).unwrap_or(b"");
                let evt = fix::get(m, tag::BOARD_EVT_ID).unwrap_or(b"");
                let text = match self.trigger.on_k04(market, evt, s.name()) {
                    K04Outcome::Armed(mk) => format!("K04 {} {} qua {}: BẮN lệnh {}", mk.id(), String::from_utf8_lossy(evt), s.name(), mk.exchange()),
                    K04Outcome::TooEarly(mk) => format!("K04 {} {} qua {} tới quá sớm so với giờ mở dự kiến: bỏ qua", mk.id(), String::from_utf8_lossy(evt), s.name()),
                    K04Outcome::AlreadyArmed | K04Outcome::NotTrigger => return,
                };
                println!("{} {text}", clock::local_time_us(clock::now_nanos(), self.utc_offset));
                self.journal.line(&text);
            }
            msg::EXEC_REPORT => self.tracker.on_exec_report(m),
            msg::BUSINESS_REJECT => self.tracker.on_business_reject(m),
            msg::REJECT => self.tracker.on_session_reject(s.idx, m),
            _ => {}
        }
    }
}

fn plan_csv(plan: &plan::Plan, cfg: &Config) -> String {
    let mut out = String::from("rank,cl_ord_id,session,line,received_at_ns,account,market,board,symbol,side,ord_type,qty,price\n");
    for (rank, o) in plan.sorted.iter().enumerate() {
        let _ = writeln!(
            out,
            "{},{},{},{},{},{},{},{},{},{},{:?},{},{}",
            rank + 1,
            o.cl_ord_id,
            cfg.sessions[plan.session_of[rank]].name,
            o.line,
            o.received_at.map(|t| t.to_string()).unwrap_or_default(),
            o.account,
            o.market.id(),
            o.board,
            o.symbol,
            o.side.wire() as char,
            o.kind,
            o.qty,
            o.price
        );
    }
    out
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn summarize(cfg: &Config, plan: &plan::Plan, tracker: &Tracker, trigger: &Trigger, markets: &[Market], unsent: usize) -> bool {
    let snap = tracker.snapshot();
    println!("\n===== KẾT QUẢ =====");
    for &m in markets {
        let ranks: Vec<usize> = (0..plan.sorted.len()).filter(|&r| plan.sorted[r].market == m).collect();
        let Some(info) = trigger.info(m) else {
            println!("{}: không mở phiên (không có K04, không có dự phòng) — {} lệnh không gửi", m.id(), ranks.len());
            continue;
        };
        let source = match &info.source {
            Source::K04 { session, evt } => format!("K04 {evt} qua {session}"),
            Source::Clock => "đồng hồ (dự phòng)".to_string(),
        };
        let mut sent: Vec<Instant> = ranks.iter().filter_map(|&r| snap.sent_at[r]).collect();
        sent.sort();
        let delays = match (sent.first(), sent.last()) {
            (Some(first), Some(last)) => format!(
                "lệnh đầu rời socket sau {}, lệnh cuối sau {}",
                clock::fmt_dur(first.saturating_duration_since(info.at)),
                clock::fmt_dur(last.saturating_duration_since(info.at))
            ),
            _ => "chưa gửi lệnh nào".into(),
        };
        let vs_open = (info.wall - cfg.open_at) as f64 / 1e6;
        println!("{}: kích hoạt bởi {source} lúc {} ({vs_open:+.1}ms so với giờ dự kiến); {delays}", m.id(), clock::local_time_us(info.wall, cfg.utc_offset));
    }

    let mut all_ok = unsent == 0;
    for (i, s) in cfg.sessions.iter().enumerate() {
        let ranks = &plan.per_session[i];
        if ranks.is_empty() {
            continue;
        }
        let count = |st: State| ranks.iter().filter(|&&r| snap.state[r] == st).count();
        let (acc, rej, no_resp, pending) = (count(State::Accepted), count(State::Rejected), count(State::Sent), count(State::Pending));
        let mut rtt: Vec<Duration> = ranks.iter().filter_map(|&r| Some(snap.resp_at[r]?.saturating_duration_since(snap.sent_at[r]?))).collect();
        rtt.sort();
        println!(
            "session {:<10} gửi {}/{}: {} được nhận, {} bị từ chối, {} không phản hồi; phản hồi đầu p50 {} p99 {} max {}",
            s.name,
            ranks.len() - pending,
            ranks.len(),
            acc,
            rej,
            no_resp,
            clock::fmt_dur(percentile(&rtt, 0.5)),
            clock::fmt_dur(percentile(&rtt, 0.99)),
            clock::fmt_dur(rtt.last().copied().unwrap_or_default())
        );
        all_ok &= rej == 0 && no_resp == 0 && pending == 0;
    }

    let rejected: Vec<usize> = (0..plan.sorted.len()).filter(|&r| snap.state[r] == State::Rejected).collect();
    for &r in rejected.iter().take(20) {
        let o = &plan.sorted[r];
        println!("  TỪ CHỐI {} dòng {} {} {}: {}", o.cl_ord_id, o.line, o.account, o.symbol, snap.reason[r]);
    }
    if rejected.len() > 20 {
        println!("  ... và {} lệnh bị từ chối khác (xem journal)", rejected.len() - 20);
    }
    if unsent > 0 {
        println!("{unsent} lệnh chưa gửi");
    }
    all_ok
}
