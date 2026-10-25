//! Cấu hình qua biến môi trường (hoặc file .env). Tên biến xem README.

use std::path::{Path, PathBuf};
use std::time::Duration;
use std::{env, fs, io};

use crate::clock;
use crate::market::Market;

#[derive(Clone, Debug)]
pub struct SessionCfg {
    pub name: String,
    pub addr: String,
    pub sender: String,
    pub target: String,
    /// Thị trường session này gửi lệnh. Rỗng = session chỉ nghe tin (K04).
    pub markets: Vec<Market>,
    /// Số lệnh/giây tối đa sở cho phép trên session (để thấp hơn hạn mức thật ~5-10%).
    pub rate: f64,
    pub burst: u32,
    /// Số lệnh tối đa gộp vào một lần write (1 = mỗi lệnh một syscall).
    pub batch: u32,
    /// Thread đọc spin non-blocking trong cửa sổ đua (chiếm một core) thay vì chờ kernel đánh thức.
    /// Chỉ cần cho session nhận K04; session chỉ nhận ER thì tắt để nhường core.
    pub busy_poll: bool,
    /// Gửi 141=Y lúc logon và bắt đầu từ seq 1 (sở/simulator để ResetOnLogon=Y).
    pub reset_seq: bool,
}

impl SessionCfg {
    pub fn serves(&self, m: Market) -> bool {
        self.markets.contains(&m)
    }
}

#[derive(Debug)]
pub struct Config {
    /// Giờ mở phiên dự kiến (unix nano): mốc để kết nối, bắt đầu spin, chạy dự phòng và bỏ cuộc.
    pub open_at: i64,
    pub utc_offset: i32,
    pub connect_lead: Duration,
    pub spin_lead: Duration,
    /// K04 tới sớm hơn open_at - k04_earliest bị bỏ qua (tin cũ/nhầm).
    pub k04_earliest: Duration,
    /// Hết thời gian này sau open_at mà chưa có K04 thì bắn theo đồng hồ. None = chỉ chờ K04.
    pub fallback_delay: Option<Duration>,
    pub give_up_after: Duration,
    pub ack_timeout: Duration,
    pub heartbeat: Duration,
    pub orders_file: PathBuf,
    pub journal_dir: PathBuf,
    pub clordid_prefix: String,
    pub begin_string: String,
    pub sender_sub_id: Option<String>,
    pub check_tick: bool,
    /// BoardEvtID (tag 20005) của K04 mở phiên, theo thị trường (chỉ số Market::index).
    pub triggers: [Vec<String>; 3],
    pub sessions: Vec<SessionCfg>,
}

struct Env<'a> {
    get: &'a dyn Fn(&str) -> Option<String>,
    errors: Vec<String>,
}

impl Env<'_> {
    fn str(&self, key: &str) -> Option<String> {
        (self.get)(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
    }

    fn or(&self, key: &str, default: &str) -> String {
        self.str(key).unwrap_or_else(|| default.to_string())
    }

    fn required(&mut self, key: &str) -> String {
        self.str(key).unwrap_or_else(|| {
            self.errors.push(format!("{key} là bắt buộc"));
            String::new()
        })
    }

    fn duration(&mut self, key: &str, default: &str) -> Duration {
        self.opt_duration(key).unwrap_or_else(|| clock::parse_duration(default).unwrap())
    }

    fn opt_duration(&mut self, key: &str) -> Option<Duration> {
        let v = self.str(key)?;
        match clock::parse_duration(&v) {
            Ok(d) => Some(d),
            Err(e) => {
                self.errors.push(format!("{key}: {e}"));
                None
            }
        }
    }

    fn parse<T: std::str::FromStr>(&mut self, key: &str, default: T) -> T {
        match self.str(key) {
            None => default,
            Some(v) => v.parse().unwrap_or_else(|_| {
                self.errors.push(format!("{key}: giá trị không hợp lệ {v:?}"));
                default
            }),
        }
    }

    fn flag(&mut self, key: &str, default: bool) -> bool {
        match self.str(key).map(|v| v.to_ascii_lowercase()) {
            None => default,
            Some(v) if ["1", "true", "y", "yes"].contains(&v.as_str()) => true,
            Some(v) if ["0", "false", "n", "no"].contains(&v.as_str()) => false,
            Some(v) => {
                self.errors.push(format!("{key}: cần true/false, nhận {v:?}"));
                default
            }
        }
    }
}

fn list(v: &str) -> impl Iterator<Item = &str> {
    v.split(',').map(str::trim).filter(|s| !s.is_empty())
}

pub fn load(get: &dyn Fn(&str) -> Option<String>, now: i64) -> Result<Config, String> {
    let mut e = Env { get, errors: Vec::new() };

    let utc_offset = clock::parse_offset(&e.or("RACE_UTC_OFFSET", "+07:00")).unwrap_or_else(|err| {
        e.errors.push(format!("RACE_UTC_OFFSET: {err}"));
        7 * 3600
    });
    let open = e.required("RACE_OPEN_TIME");
    let open_at = if open.is_empty() {
        0
    } else {
        clock::parse_open_time(&open, now, utc_offset).unwrap_or_else(|err| {
            e.errors.push(format!("RACE_OPEN_TIME: {err}"));
            0
        })
    };

    let defaults = [("STO", "AA1"), ("STX", "BB1"), ("UPX", "AB1")];
    let triggers = defaults.map(|(m, d)| list(&e.or(&format!("RACE_TRIGGER_{m}"), d)).map(String::from).collect());

    let busy_poll_default = e.flag("RACE_BUSY_POLL", true);
    let mut sessions = Vec::new();
    for name in list(&e.required("RACE_SESSIONS")).map(String::from).collect::<Vec<_>>() {
        if !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            e.errors.push(format!("tên session {name:?} chỉ được gồm chữ, số, '_'"));
            continue;
        }
        let key = name.to_ascii_uppercase();
        if sessions.iter().any(|s: &SessionCfg| s.name.eq_ignore_ascii_case(&name)) {
            e.errors.push(format!("session {name} bị khai báo hai lần"));
            continue;
        }
        let p = |field: &str| format!("RACE_{key}_{field}");
        let mut markets = Vec::new();
        for m in list(&e.or(&p("MARKETS"), "")) {
            match Market::parse(m) {
                Some(m) if !markets.contains(&m) => markets.push(m),
                Some(_) => {}
                None => e.errors.push(format!("{}: thị trường không hợp lệ {m:?}", p("MARKETS"))),
            }
        }
        let s = SessionCfg {
            addr: e.required(&p("ADDR")),
            sender: e.required(&p("SENDER")),
            target: e.required(&p("TARGET")),
            markets,
            rate: e.parse(&p("RATE"), 50.0),
            burst: e.parse(&p("BURST"), 1),
            batch: e.parse(&p("BATCH"), 32),
            busy_poll: e.flag(&p("BUSY_POLL"), busy_poll_default),
            reset_seq: e.flag(&p("RESET_SEQ"), false),
            name,
        };
        if s.rate <= 0.0 || s.burst == 0 || s.batch == 0 {
            e.errors.push(format!("{}, {} và {} phải > 0", p("RATE"), p("BURST"), p("BATCH")));
        }
        sessions.push(s);
    }

    let cfg = Config {
        open_at,
        utc_offset,
        connect_lead: e.duration("RACE_CONNECT_LEAD", "2m"),
        spin_lead: e.duration("RACE_SPIN_LEAD", "3s"),
        k04_earliest: e.duration("RACE_K04_EARLIEST", "10m"),
        fallback_delay: e.opt_duration("RACE_FALLBACK_DELAY"),
        give_up_after: e.duration("RACE_GIVE_UP_AFTER", "5m"),
        ack_timeout: e.duration("RACE_ACK_TIMEOUT", "15s"),
        heartbeat: e.duration("RACE_HEARTBEAT", "30s"),
        orders_file: e.or("RACE_ORDERS_FILE", "orders.csv").into(),
        journal_dir: e.or("RACE_JOURNAL_DIR", "journal").into(),
        clordid_prefix: e.or("RACE_CLORDID_PREFIX", "R"),
        begin_string: e.or("RACE_BEGIN_STRING", "FIX.4.4"),
        sender_sub_id: e.str("RACE_SENDER_SUB_ID").or_else(|| (get)("RACE_SENDER_SUB_ID").is_none().then(|| "HSC".into())),
        check_tick: e.flag("RACE_CHECK_TICK", true),
        triggers,
        sessions,
    };
    if cfg.heartbeat < Duration::from_secs(1) {
        e.errors.push("RACE_HEARTBEAT phải >= 1s".into());
    }
    if e.errors.is_empty() {
        Ok(cfg)
    } else {
        Err(format!("cấu hình không hợp lệ:\n  {}", e.errors.join("\n  ")))
    }
}

/// Nạp file KEY=VALUE vào môi trường; biến đã có sẵn được giữ nguyên. Gọi trước khi tạo thread.
pub fn load_dotenv(path: &Path) -> io::Result<usize> {
    let text = fs::read_to_string(path)?;
    let mut n = 0;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((k, v)) = line.split_once('=') else { continue };
        let (k, mut v) = (k.trim(), v.trim());
        if v.len() >= 2 && (v.starts_with('"') && v.ends_with('"') || v.starts_with('\'') && v.ends_with('\'')) {
            v = &v[1..v.len() - 1];
        }
        if env::var_os(k).is_none() {
            env::set_var(k, v);
            n += 1;
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn load_map(pairs: &[(&str, &str)]) -> Result<Config, String> {
        let m: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let now = clock::parse_rfc3339("2026-10-25T08:30:00+07:00").unwrap();
        load(&move |k| m.get(k).cloned(), now)
    }

    const BASE: &[(&str, &str)] = &[
        ("RACE_OPEN_TIME", "09:00:00"),
        ("RACE_SESSIONS", "hose1, hose_md"),
        ("RACE_HOSE1_ADDR", "127.0.0.1:30120"),
        ("RACE_HOSE1_SENDER", "HOSE01"),
        ("RACE_HOSE1_TARGET", "LTHOSE"),
        ("RACE_HOSE1_MARKETS", "HOSE"),
        ("RACE_HOSE1_RATE", "200"),
        ("RACE_HOSE_MD_ADDR", "127.0.0.1:30119"),
        ("RACE_HOSE_MD_SENDER", "HOSE02"),
        ("RACE_HOSE_MD_TARGET", "VOMGW"),
    ];

    #[test]
    fn loads_sessions_and_defaults() {
        let c = load_map(BASE).unwrap();
        assert_eq!(c.open_at, clock::parse_rfc3339("2026-10-25T09:00:00+07:00").unwrap());
        assert_eq!(c.sessions.len(), 2);
        assert_eq!(c.sessions[0].markets, vec![Market::Sto]);
        assert_eq!(c.sessions[0].rate, 200.0);
        assert!(c.sessions[1].markets.is_empty());
        assert_eq!(c.sessions[1].rate, 50.0);
        assert_eq!(c.triggers[Market::Sto.index()], vec!["AA1"]);
        assert_eq!(c.sender_sub_id.as_deref(), Some("HSC"));
        assert_eq!(c.fallback_delay, None);
    }

    #[test]
    fn empty_sender_sub_id_disables_it() {
        let mut v = BASE.to_vec();
        v.push(("RACE_SENDER_SUB_ID", ""));
        assert_eq!(load_map(&v).unwrap().sender_sub_id, None);
    }

    #[test]
    fn reports_all_errors() {
        let err = load_map(&[
            ("RACE_SESSIONS", "a,a,b-c"),
            ("RACE_A_MARKETS", "HOSE,NYSE"),
            ("RACE_A_RATE", "0"),
            ("RACE_FALLBACK_DELAY", "soon"),
        ])
        .unwrap_err();
        for want in ["RACE_OPEN_TIME", "RACE_A_ADDR", "hai lần", "b-c", "NYSE", "RACE_A_RATE", "RACE_FALLBACK_DELAY"] {
            assert!(err.contains(want), "thiếu {want:?} trong:\n{err}");
        }
    }
}
