//! Thread gửi của một session: chờ cờ kích hoạt của thị trường rồi xả hàng lệnh theo thứ tự
//! ưu tiên, giữ đúng throttle. Một session có thể phục vụ nhiều thị trường (vd HNX + UPCoM):
//! thị trường nào mở trước thì lệnh của nó đi trước, lệnh của thị trường chưa mở vẫn chờ.
//!
//! Gộp lệnh: mỗi lần ghi socket tốn một syscall (trên Windows loopback cỡ 20-40µs), nên các lệnh
//! đang có quota throttle được mã hoá liền vào một buffer và gửi bằng một lần write.

use std::hint;
use std::thread;
use std::time::{Duration, Instant};

use crate::fix::OrderTemplate;
use crate::market::Market;
use crate::session::Session;
use crate::throttle::Bucket;
use crate::track::SentRec;
use crate::trigger::Trigger;

pub struct FireWindow {
    /// Từ lúc này thread spin trên cờ thay vì ngủ 1ms.
    pub spin_from: Instant,
    /// Quá lúc này mà còn lệnh chưa gửi được (thị trường không mở) thì bỏ cuộc.
    pub give_up_at: Instant,
}

pub struct Outcome {
    pub sent: Vec<SentRec>,
    pub unsent: usize,
    pub error: Option<String>,
}

/// Hàng của thị trường đã mở có lệnh đứng đầu với rank nhỏ nhất.
#[inline]
fn pick(armed: u32, queues: &[Vec<OrderTemplate>], pos: &[usize; 3]) -> Option<usize> {
    Market::ALL
        .iter()
        .map(|m| m.index())
        .filter(|&q| armed & (1 << q) != 0 && pos[q] < queues[q].len())
        .min_by_key(|&q| queues[q][pos[q]].rank)
}

pub fn run(session: &Session, orders: Vec<OrderTemplate>, trigger: &Trigger, window: FireWindow) -> Outcome {
    let total = orders.len();
    let mut queues: Vec<Vec<OrderTemplate>> = Market::ALL.iter().map(|_| Vec::new()).collect();
    for t in orders {
        queues[t.market.index()].push(t); // orders đã theo rank tăng dần
    }
    let mut pos = [0usize; 3];
    let mut bucket = Bucket::new(session.cfg.rate, session.cfg.burst);
    let max_batch = session.cfg.batch.max(1) as usize;
    let mut sent = Vec::with_capacity(total);
    let mut batch_ranks: Vec<usize> = Vec::with_capacity(max_batch);

    loop {
        if pick(trigger.armed_bits(), &queues, &pos).is_none() {
            if sent.len() == total {
                break;
            }
            let now = Instant::now();
            if now >= window.give_up_at {
                return Outcome { unsent: total - sent.len(), sent, error: Some("hết giờ chờ mở phiên".into()) };
            }
            if !session.is_alive() {
                return Outcome { unsent: total - sent.len(), sent, error: Some("session đã ngắt trước khi mở phiên".into()) };
            }
            if now >= window.spin_from {
                hint::spin_loop();
            } else {
                thread::sleep(Duration::from_millis(1));
            }
            continue;
        }

        bucket.wait();
        batch_ranks.clear();
        let res = session.send_batch(|buf, hdr, first_seq, ts| {
            let mut n = 0u64;
            while let Some(q) = pick(trigger.armed_bits(), &queues, &pos) {
                // lệnh đầu dùng quota của bucket.wait(); lệnh sau chỉ đi kèm nếu quota có sẵn
                if n > 0 && (batch_ranks.len() == max_batch || !bucket.try_take()) {
                    break;
                }
                let t = &mut queues[q][pos[q]];
                t.encode(buf, hdr, first_seq + n, ts);
                batch_ranks.push(t.rank);
                pos[q] += 1;
                n += 1;
            }
            n
        });
        match res {
            Ok((first_seq, _)) => {
                let at = Instant::now();
                sent.extend(batch_ranks.iter().enumerate().map(|(k, &rank)| SentRec { rank, seq: first_seq + k as u64, at }));
            }
            Err(e) => {
                return Outcome { unsent: total - sent.len(), sent, error: Some(format!("ghi socket lỗi: {e}")) };
            }
        }
    }
    Outcome { sent, unsent: 0, error: None }
}
