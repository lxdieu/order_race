//! Giới hạn tốc độ GCRA cho một thread gửi: cho phép `burst` lệnh liền nhau rồi giữ đều `rate`
//! lệnh/giây. Vượt hạn mức của sở có thể bị ngắt session, nên đặt rate thấp hơn hạn mức thật.

use std::hint;
use std::thread;
use std::time::{Duration, Instant};

pub struct Bucket {
    interval: Duration,
    tolerance: Duration,
    tat: Option<Instant>, // theoretical arrival time
}

impl Bucket {
    pub fn new(rate: f64, burst: u32) -> Self {
        let interval = Duration::from_secs_f64(1.0 / rate);
        Bucket { interval, tolerance: interval * burst.saturating_sub(1), tat: None }
    }

    /// Chờ tới khi được gửi lệnh kế tiếp: ngủ nếu còn lâu, spin đoạn cuối.
    pub fn wait(&mut self) {
        let now = Instant::now();
        let tat = self.tat.map_or(now, |t| t.max(now));
        if let Some(allow_at) = tat.checked_sub(self.tolerance) {
            if allow_at > now {
                let left = allow_at - now;
                if left > Duration::from_millis(2) {
                    thread::sleep(left - Duration::from_millis(1));
                }
                while Instant::now() < allow_at {
                    hint::spin_loop();
                }
            }
        }
        self.tat = Some(tat + self.interval);
    }

    /// Lấy quota nếu được gửi ngay bây giờ; không chờ.
    pub fn try_take(&mut self) -> bool {
        let now = Instant::now();
        let tat = self.tat.map_or(now, |t| t.max(now));
        if tat.checked_sub(self.tolerance).is_some_and(|allow_at| allow_at > now) {
            return false;
        }
        self.tat = Some(tat + self.interval);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paces_after_burst() {
        let mut b = Bucket::new(1000.0, 5);
        let start = Instant::now();
        for _ in 0..5 {
            b.wait();
        }
        assert!(start.elapsed() < Duration::from_millis(2), "burst phải đi ngay");
        for _ in 0..20 {
            b.wait();
        }
        // 5 lệnh burst + 20 lệnh cách đều 1ms: lệnh cuối không sớm hơn ~20ms
        assert!(start.elapsed() >= Duration::from_millis(19), "{:?}", start.elapsed());
    }

    #[test]
    fn try_take_respects_burst() {
        let mut b = Bucket::new(10.0, 3);
        b.wait();
        assert!(b.try_take());
        assert!(b.try_take());
        assert!(!b.try_take(), "hết burst thì phải chờ 100ms");
    }
}
