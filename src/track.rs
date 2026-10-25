//! Theo dõi phản hồi đầu tiên của sở cho từng lệnh (ack hay từ chối) để tổng kết và đối soát.

use std::collections::HashMap;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::fix::{self, tag};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Pending,
    Sent,
    Accepted,
    Rejected,
}

pub struct SentRec {
    pub rank: usize,
    pub seq: u64,
    pub at: Instant,
}

struct Inner {
    state: Vec<State>,
    reason: Vec<String>,
    seq: Vec<u64>,
    sent_at: Vec<Option<Instant>>,
    resp_at: Vec<Option<Instant>>,
    sent: usize,
    responded: usize,
}

pub struct Tracker {
    index: HashMap<String, usize>,
    session_of: Vec<usize>,
    inner: Mutex<Inner>,
    cv: Condvar,
}

/// Ảnh chụp để in tổng kết.
pub struct Snapshot {
    pub state: Vec<State>,
    pub reason: Vec<String>,
    pub sent_at: Vec<Option<Instant>>,
    pub resp_at: Vec<Option<Instant>>,
}

impl Tracker {
    pub fn new(cl_ord_ids: Vec<String>, session_of: Vec<usize>) -> Self {
        let n = cl_ord_ids.len();
        Tracker {
            index: cl_ord_ids.into_iter().enumerate().map(|(i, id)| (id, i)).collect(),
            session_of,
            inner: Mutex::new(Inner {
                state: vec![State::Pending; n],
                reason: vec![String::new(); n],
                seq: vec![0; n],
                sent_at: vec![None; n],
                resp_at: vec![None; n],
                sent: 0,
                responded: 0,
            }),
            cv: Condvar::new(),
        }
    }

    /// Ghi lại các lệnh một session đã gửi (gọi sau khi burst xong, ngoài đường gửi).
    pub fn record_sent(&self, recs: &[SentRec]) {
        let mut g = self.inner.lock().unwrap();
        for r in recs {
            g.seq[r.rank] = r.seq;
            g.sent_at[r.rank] = Some(r.at);
            if g.state[r.rank] == State::Pending {
                g.state[r.rank] = State::Sent;
            }
        }
        g.sent += recs.len();
        self.cv.notify_all();
    }

    pub fn on_exec_report(&self, msg: &[u8]) {
        let Some(&rank) = std::str::from_utf8(fix::get(msg, tag::CL_ORD_ID).unwrap_or(b"")).ok().and_then(|id| self.index.get(id)) else {
            return;
        };
        let rejected = fix::get(msg, tag::ORD_STATUS) == Some(b"8");
        let reason = if rejected {
            format!("8/8 103={} {}", fix::get_str(msg, tag::ORD_REJ_REASON), fix::get_str(msg, tag::TEXT))
        } else {
            String::new()
        };
        self.respond(rank, rejected, reason);
    }

    pub fn on_business_reject(&self, msg: &[u8]) {
        if let Some(&rank) = std::str::from_utf8(fix::get(msg, tag::BUSINESS_REJECT_REF_ID).unwrap_or(b"")).ok().and_then(|id| self.index.get(id)) {
            self.respond(rank, true, format!("35=j {}", fix::get_str(msg, tag::TEXT)));
        }
    }

    /// 35=3 chỉ có RefSeqNum: tìm lệnh của session này có MsgSeqNum đó.
    pub fn on_session_reject(&self, session: usize, msg: &[u8]) {
        let Some(ref_seq) = fix::get_u64(msg, tag::REF_SEQ_NUM) else { return };
        let rank = {
            let g = self.inner.lock().unwrap();
            (0..g.seq.len()).find(|&r| self.session_of[r] == session && g.seq[r] == ref_seq)
        };
        if let Some(rank) = rank {
            self.respond(rank, true, format!("35=3 {}", fix::get_str(msg, tag::TEXT)));
        }
    }

    fn respond(&self, rank: usize, rejected: bool, reason: String) {
        let now = Instant::now();
        let mut g = self.inner.lock().unwrap();
        if g.resp_at[rank].is_some() {
            return; // chỉ tính phản hồi đầu tiên (fill sau ack bỏ qua)
        }
        g.resp_at[rank] = Some(now);
        g.state[rank] = if rejected { State::Rejected } else { State::Accepted };
        g.reason[rank] = reason;
        g.responded += 1;
        self.cv.notify_all();
    }

    /// Chờ tới khi mọi lệnh đã gửi có phản hồi. Trả false nếu hết giờ.
    pub fn wait(&self, timeout: Duration) -> bool {
        let g = self.inner.lock().unwrap();
        let (g, res) = self.cv.wait_timeout_while(g, timeout, |g| g.responded < g.sent).unwrap();
        drop(g);
        !res.timed_out()
    }

    pub fn snapshot(&self) -> Snapshot {
        let g = self.inner.lock().unwrap();
        Snapshot { state: g.state.clone(), reason: g.reason.clone(), sent_at: g.sent_at.clone(), resp_at: g.resp_at.clone() }
    }
}
