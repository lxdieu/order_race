//! Cờ kích hoạt theo thị trường. Thread đọc FIX bật cờ khi nhận K04 mở phiên; thread gửi spin
//! trên cờ (một lần đọc atomic) nên phản ứng trong cỡ trăm nano giây.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use crate::clock;
use crate::market::Market;

#[derive(Clone, Debug)]
pub enum Source {
    K04 { session: String, evt: String },
    Clock,
}

#[derive(Clone, Debug)]
pub struct Armed {
    pub at: Instant,
    pub wall: i64,
    pub source: Source,
}

pub enum K04Outcome {
    Armed(Market),
    AlreadyArmed,
    TooEarly(Market),
    NotTrigger,
}

pub struct Trigger {
    bits: AtomicU32,
    events: [Vec<String>; 3],
    /// K04 trước thời điểm này (unix nano) bị bỏ qua.
    earliest: i64,
    info: Mutex<[Option<Armed>; 3]>,
}

impl Trigger {
    pub fn new(events: [Vec<String>; 3], earliest: i64) -> Self {
        Trigger { bits: AtomicU32::new(0), events, earliest, info: Mutex::new([None, None, None]) }
    }

    #[inline]
    pub fn armed_bits(&self) -> u32 {
        self.bits.load(Ordering::Acquire)
    }

    pub fn on_k04(&self, market: &[u8], evt: &[u8], session: &str) -> K04Outcome {
        let Some(m) = Market::from_wire(market) else { return K04Outcome::NotTrigger };
        if !self.events[m.index()].iter().any(|e| e.as_bytes() == evt) {
            return K04Outcome::NotTrigger;
        }
        if clock::now_nanos() < self.earliest {
            return K04Outcome::TooEarly(m);
        }
        let armed = self.arm_with(m, || Source::K04 { session: session.to_string(), evt: String::from_utf8_lossy(evt).into_owned() });
        if armed { K04Outcome::Armed(m) } else { K04Outcome::AlreadyArmed }
    }

    /// Bật cờ của thị trường. Trả true nếu đây là lần bật đầu tiên.
    pub fn arm(&self, m: Market, source: Source) -> bool {
        self.arm_with(m, || source)
    }

    /// Bật cờ trước, mọi việc khác (đọc đồng hồ, cấp phát Source, lock) làm sau khi thread gửi đã thấy cờ.
    fn arm_with(&self, m: Market, source: impl FnOnce() -> Source) -> bool {
        if self.bits.fetch_or(m.bit(), Ordering::AcqRel) & m.bit() != 0 {
            return false;
        }
        let (at, wall) = (Instant::now(), clock::now_nanos());
        self.info.lock().unwrap()[m.index()] = Some(Armed { at, wall, source: source() });
        true
    }

    pub fn info(&self, m: Market) -> Option<Armed> {
        self.info.lock().unwrap()[m.index()].clone()
    }

    /// Hạ mọi cờ (dùng cho bài đo chạy nhiều vòng).
    pub fn reset(&self) {
        *self.info.lock().unwrap() = [None, None, None];
        self.bits.store(0, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trigger(earliest: i64) -> Trigger {
        Trigger::new([vec!["AA1".into()], vec!["BB1".into()], vec!["AB1".into()]], earliest)
    }

    #[test]
    fn arms_once_on_matching_event() {
        let t = trigger(0);
        assert!(matches!(t.on_k04(b"STO", b"BB1", "md"), K04Outcome::NotTrigger));
        assert!(matches!(t.on_k04(b"STX", b"AA1", "md"), K04Outcome::NotTrigger));
        assert!(matches!(t.on_k04(b"XXX", b"AA1", "md"), K04Outcome::NotTrigger));
        assert_eq!(t.armed_bits(), 0);
        assert!(matches!(t.on_k04(b"STO", b"AA1", "md"), K04Outcome::Armed(Market::Sto)));
        assert!(matches!(t.on_k04(b"STO", b"AA1", "md"), K04Outcome::AlreadyArmed));
        assert_eq!(t.armed_bits(), Market::Sto.bit());
        assert!(matches!(t.info(Market::Sto).unwrap().source, Source::K04 { .. }));
        assert!(!t.arm(Market::Sto, Source::Clock));
    }

    #[test]
    fn ignores_k04_before_earliest() {
        let t = trigger(i64::MAX);
        assert!(matches!(t.on_k04(b"UPX", b"AB1", "md"), K04Outcome::TooEarly(Market::Upx)));
        assert_eq!(t.armed_bits(), 0);
    }
}
