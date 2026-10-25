//! Xếp thứ tự gửi và chia lệnh cho các session.
//!
//! Quy tắc công bằng: lệnh nhận trước đi trước (received_at, cùng thời điểm thì theo dòng trong
//! file; lệnh không có received_at đứng sau cùng). Mỗi lệnh vào session phục vụ đúng thị trường
//! mà hàng chờ dự kiến xả xong sớm nhất (số lệnh đã xếp / rate), nên thứ tự chung được giữ gần
//! nhất có thể khi gửi song song nhiều session.

use crate::config::SessionCfg;
use crate::order::Order;

pub struct Plan {
    /// Lệnh đã định tuyến, theo thứ tự ưu tiên. Chỉ số trong vec là `rank` của lệnh.
    pub sorted: Vec<Order>,
    /// session_of[rank] = chỉ số session gửi lệnh đó.
    pub session_of: Vec<usize>,
    /// per_session[s] = các rank session s gửi, tăng dần.
    pub per_session: Vec<Vec<usize>>,
    pub unroutable: Vec<Order>,
}

pub fn build(mut orders: Vec<Order>, sessions: &[SessionCfg]) -> Plan {
    orders.sort_by_key(|o| (o.received_at.unwrap_or(i64::MAX), o.line));
    let mut plan = Plan { sorted: Vec::new(), session_of: Vec::new(), per_session: vec![Vec::new(); sessions.len()], unroutable: Vec::new() };
    for o in orders {
        let best = sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| s.serves(o.market))
            .map(|(i, s)| (i, plan.per_session[i].len() as f64 / s.rate))
            .min_by(|a, b| a.1.total_cmp(&b.1));
        match best {
            Some((i, _)) => {
                let rank = plan.sorted.len();
                plan.per_session[i].push(rank);
                plan.session_of.push(i);
                plan.sorted.push(o);
            }
            None => plan.unroutable.push(o),
        }
    }
    plan
}

impl Plan {
    /// ClOrdID = prefix + YYMMDD + số thứ tự 6 chữ số theo rank. Chạy lại trong ngày sinh lại
    /// đúng các ClOrdID cũ, nên sở từ chối trùng thay vì nhận một lệnh hai lần.
    pub fn assign_cl_ord_ids(&mut self, prefix: &str, yyyymmdd: &str) {
        let yymmdd = &yyyymmdd[2..];
        for (rank, o) in self.sorted.iter_mut().enumerate() {
            o.cl_ord_id = format!("{prefix}{yymmdd}{:06}", rank + 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market::Market;
    use crate::order::load_csv;

    fn session(name: &str, markets: &[Market], rate: f64) -> SessionCfg {
        SessionCfg {
            name: name.into(),
            addr: String::new(),
            sender: String::new(),
            target: String::new(),
            markets: markets.to_vec(),
            rate,
            burst: 1,
            batch: 32,
            busy_poll: true,
            reset_seq: true,
        }
    }

    #[test]
    fn fifo_and_balanced() {
        let (orders, errors) = load_csv(
            "received_at,account,exchange,symbol,side,ord_type,qty,price
2026-10-24T20:00:03+07:00,A3,HOSE,X,B,LO,100,24200
2026-10-24T20:00:01+07:00,A1,HOSE,X,B,LO,100,24200
,A6,HOSE,X,B,LO,100,24200
2026-10-24T20:00:02+07:00,A2,HNX,Y,B,LO,100,10000
2026-10-24T20:00:04+07:00,A4,HOSE,X,B,LO,100,24200
2026-10-24T20:00:05+07:00,A5,HOSE,X,B,LO,100,24200
2026-10-24T20:00:06+07:00,A7,UPCOM,Z,B,LO,100,10000
",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let sessions = [
            session("hose1", &[Market::Sto], 100.0),
            session("hose2", &[Market::Sto], 100.0),
            session("hnx", &[Market::Stx], 100.0),
            session("md", &[], 100.0),
        ];
        let mut p = build(orders, &sessions);
        let accounts: Vec<_> = p.sorted.iter().map(|o| o.account.as_str()).collect();
        assert_eq!(accounts, ["A1", "A2", "A3", "A4", "A5", "A6"]);
        assert_eq!(p.unroutable.len(), 1); // UPCOM không có session
        // HOSE luân phiên giữa hai session theo thứ tự ưu tiên
        assert_eq!(p.per_session[0], [0, 3, 5]);
        assert_eq!(p.per_session[1], [2, 4]);
        assert_eq!(p.per_session[2], [1]);
        assert!(p.per_session[3].is_empty());
        assert_eq!(p.session_of, [0, 2, 1, 0, 1, 0]);
        p.assign_cl_ord_ids("R", "20261025");
        assert_eq!(p.sorted[0].cl_ord_id, "R261025000001");
        assert_eq!(p.sorted[5].cl_ord_id, "R261025000006");
    }

    #[test]
    fn faster_session_takes_more() {
        let csv: String = std::iter::once("account,exchange,symbol,side,ord_type,qty,price\n".to_string())
            .chain((0..30).map(|i| format!("A{i},HOSE,X,B,LO,100,24200\n")))
            .collect();
        let (orders, _) = load_csv(&csv);
        let sessions = [session("fast", &[Market::Sto], 200.0), session("slow", &[Market::Sto], 100.0)];
        let p = build(orders, &sessions);
        assert_eq!(p.per_session[0].len(), 20);
        assert_eq!(p.per_session[1].len(), 10);
    }
}
