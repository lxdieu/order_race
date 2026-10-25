//! FIX 4.4 tối giản cho racer, gồm cả tag KRX custom mà LT-OMS gửi. Encode ghi thẳng vào
//! buffer của người gọi để đường gửi lệnh không cấp phát bộ nhớ.

use std::io::{self, BufRead, Read};

use crate::clock::Timestamp;
use crate::market::Market;
use crate::order::{Order, OrdKind};

pub const SOH: u8 = 0x01;

pub mod tag {
    pub const ACCOUNT: u32 = 1;
    pub const BEGIN_SEQ_NO: u32 = 7;
    pub const CL_ORD_ID: u32 = 11;
    pub const HANDL_INST: u32 = 21;
    pub const MSG_SEQ_NUM: u32 = 34;
    pub const MSG_TYPE: u32 = 35;
    pub const NEW_SEQ_NO: u32 = 36;
    pub const ORDER_QTY: u32 = 38;
    pub const ORD_STATUS: u32 = 39;
    pub const ORD_TYPE: u32 = 40;
    pub const POSS_DUP_FLAG: u32 = 43;
    pub const PRICE: u32 = 44;
    pub const REF_SEQ_NUM: u32 = 45;
    pub const SENDER_SUB_ID: u32 = 50;
    pub const SIDE: u32 = 54;
    pub const SYMBOL: u32 = 55;
    pub const TEXT: u32 = 58;
    pub const TIME_IN_FORCE: u32 = 59;
    pub const TRANSACT_TIME: u32 = 60;
    pub const TRADE_DATE: u32 = 75;
    pub const OPEN_CLOSE: u32 = 77;
    pub const ENCRYPT_METHOD: u32 = 98;
    pub const ORD_REJ_REASON: u32 = 103;
    pub const HEART_BT_INT: u32 = 108;
    pub const TEST_REQ_ID: u32 = 112;
    pub const ON_BEHALF_OF_COMP_ID: u32 = 115; // K04: MarketID
    pub const ORIG_SENDING_TIME: u32 = 122;
    pub const GAP_FILL_FLAG: u32 = 123;
    pub const DELIVER_TO_COMP_ID: u32 = 128; // lệnh: MarketID
    pub const DELIVER_TO_SUB_ID: u32 = 129; // lệnh: BoardID
    pub const RESET_SEQ_NUM_FLAG: u32 = 141;
    pub const BUSINESS_REJECT_REF_ID: u32 = 379;
    pub const COUNTRY: u32 = 421;
    pub const CASH_MARGIN: u32 = 544;
    pub const ACCOUNT_TYPE: u32 = 581;
    pub const INVEST_CODE: u32 = 20000;
    pub const BOARD_EVT_ID: u32 = 20005;
    pub const FORN_INVEST_TYPE_CODE: u32 = 20054;
}

pub mod msg {
    pub const HEARTBEAT: &[u8] = b"0";
    pub const TEST_REQUEST: &[u8] = b"1";
    pub const RESEND_REQUEST: &[u8] = b"2";
    pub const REJECT: &[u8] = b"3";
    pub const SEQUENCE_RESET: &[u8] = b"4";
    pub const LOGOUT: &[u8] = b"5";
    pub const EXEC_REPORT: &[u8] = b"8";
    pub const LOGON: &[u8] = b"A";
    pub const NEW_ORDER: &[u8] = b"D";
    pub const BUSINESS_REJECT: &[u8] = b"j";
    pub const TRADING_SESSION_STATUS: &[u8] = b"K04";
}

/// Header chuẩn của một session.
pub struct Header {
    pub begin_string: String,
    pub sender: String,
    pub target: String,
}

impl Header {
    /// Ghi một message hoàn chỉnh vào dst. `body` là các field (kết thúc bằng SOH) đứng sau
    /// SendingTime(52); field header thêm (50, 128, 129, 43, 122) đặt ở đầu body.
    pub fn encode(&self, dst: &mut Vec<u8>, msg_type: &[u8], seq: u64, sending_time: &Timestamp, body: &[u8]) {
        let body_len = 3 + msg_type.len() + 1
            + 3 + digits(seq) + 1
            + 3 + self.sender.len() + 1
            + 3 + self.target.len() + 1
            + 3 + sending_time.len() + 1
            + body.len();
        let start = dst.len();
        put_raw(dst, b"8=", self.begin_string.as_bytes());
        dst.extend_from_slice(b"9=");
        put_u64(dst, body_len as u64);
        dst.push(SOH);
        put_raw(dst, b"35=", msg_type);
        dst.extend_from_slice(b"34=");
        put_u64(dst, seq);
        dst.push(SOH);
        put_raw(dst, b"49=", self.sender.as_bytes());
        put_raw(dst, b"56=", self.target.as_bytes());
        put_raw(dst, b"52=", sending_time);
        dst.extend_from_slice(body);
        let sum = checksum(&dst[start..]);
        dst.extend_from_slice(&[b'1', b'0', b'=', b'0' + sum / 100, b'0' + sum / 10 % 10, b'0' + sum % 10, SOH]);
    }
}

pub fn put_field(dst: &mut Vec<u8>, tag: u32, value: &[u8]) {
    put_u64(dst, tag as u64);
    dst.push(b'=');
    dst.extend_from_slice(value);
    dst.push(SOH);
}

pub fn put_field_u64(dst: &mut Vec<u8>, tag: u32, value: u64) {
    put_u64(dst, tag as u64);
    dst.push(b'=');
    put_u64(dst, value);
    dst.push(SOH);
}

fn put_raw(dst: &mut Vec<u8>, prefix: &[u8], value: &[u8]) {
    dst.extend_from_slice(prefix);
    dst.extend_from_slice(value);
    dst.push(SOH);
}

fn put_u64(dst: &mut Vec<u8>, mut v: u64) {
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    dst.extend_from_slice(&buf[i..]);
}

fn digits(mut v: u64) -> usize {
    let mut n = 1;
    while v >= 10 {
        v /= 10;
        n += 1;
    }
    n
}

fn checksum(b: &[u8]) -> u8 {
    b.iter().fold(0u32, |s, &c| s + c as u32) as u8
}

/// Tách message từ luồng byte. Byte đã nhận được giữ trong buffer giữa các lần đọc, nên dùng được
/// với socket non-blocking (WouldBlock giữa chừng không làm mất dữ liệu).
pub struct FrameReader {
    buf: Vec<u8>,
    start: usize,
    end: usize,
}

impl FrameReader {
    pub fn new(capacity: usize) -> Self {
        FrameReader { buf: vec![0; capacity.max(64)], start: 0, end: 0 }
    }

    /// Message hoàn chỉnh kế tiếp đã có trong buffer (đã kiểm checksum), None nếu cần đọc thêm.
    /// Slice hợp lệ tới lần gọi `fill` sau.
    pub fn next_frame(&mut self) -> io::Result<Option<&[u8]>> {
        let data = &self.buf[self.start..self.end];
        let Some(total) = frame_len(data)? else {
            return Ok(None);
        };
        if data.len() < total {
            if total > self.buf.len() {
                self.buf.resize(total.next_power_of_two(), 0);
            }
            return Ok(None);
        }
        let at = self.start;
        self.start += total;
        verify_checksum(&self.buf[at..at + total])?;
        Ok(Some(&self.buf[at..at + total]))
    }

    /// Đọc thêm một lần từ `r`. Ok(0) là đầu kia đóng kết nối.
    pub fn fill<R: Read>(&mut self, r: &mut R) -> io::Result<usize> {
        if self.start == self.end {
            self.start = 0;
            self.end = 0;
        } else if self.end == self.buf.len() {
            self.buf.copy_within(self.start..self.end, 0);
            self.end -= self.start;
            self.start = 0;
            if self.end == self.buf.len() {
                self.buf.resize(self.buf.len() * 2, 0);
            }
        }
        let n = r.read(&mut self.buf[self.end..])?;
        self.end += n;
        Ok(n)
    }
}

/// Độ dài message đứng đầu `data` (đọc từ BodyLength), None nếu chưa đủ header để biết.
fn frame_len(data: &[u8]) -> io::Result<Option<usize>> {
    let invalid = |what: String| io::Error::new(io::ErrorKind::InvalidData, what);
    if data.len() >= 2 && !data.starts_with(b"8=") {
        return Err(invalid(format!("thiếu BeginString: {}", pretty(&data[..data.len().min(40)]))));
    }
    let Some(p1) = data.iter().position(|&b| b == SOH) else { return Ok(None) };
    let rest = &data[p1 + 1..];
    if rest.len() >= 2 && !rest.starts_with(b"9=") {
        return Err(invalid(format!("thiếu BodyLength: {}", pretty(&rest[..rest.len().min(40)]))));
    }
    let Some(p2) = rest.iter().position(|&b| b == SOH) else { return Ok(None) };
    let body_len = parse_u64(&rest[2..p2])
        .filter(|&n| n > 0 && n < 1 << 20)
        .ok_or_else(|| invalid(format!("BodyLength sai: {}", pretty(&rest[..p2]))))? as usize;
    Ok(Some(p1 + 1 + p2 + 1 + body_len + 7))
}

fn verify_checksum(m: &[u8]) -> io::Result<()> {
    let (head, trailer) = m.split_at(m.len() - 7);
    if !trailer.starts_with(b"10=") || trailer[6] != SOH {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "thiếu CheckSum"));
    }
    if parse_u64(&trailer[3..6]) != Some(checksum(head) as u64) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("checksum sai: {}", pretty(m))));
    }
    Ok(())
}

/// Đọc đúng một message và kiểm tra checksum. `buf` được ghi đè.
pub fn read_message<R: BufRead>(r: &mut R, buf: &mut Vec<u8>) -> io::Result<()> {
    let invalid = |what: String| io::Error::new(io::ErrorKind::InvalidData, what);
    buf.clear();
    if r.read_until(SOH, buf)? == 0 {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    if !buf.starts_with(b"8=") || buf.last() != Some(&SOH) {
        return Err(invalid(format!("thiếu BeginString: {}", pretty(buf))));
    }
    let len_at = buf.len();
    r.read_until(SOH, buf)?;
    let len_field = &buf[len_at..];
    if !len_field.starts_with(b"9=") || len_field.last() != Some(&SOH) {
        return Err(invalid(format!("thiếu BodyLength: {}", pretty(len_field))));
    }
    let body_len = parse_u64(&len_field[2..len_field.len() - 1])
        .filter(|&n| n > 0 && n < 1 << 20)
        .ok_or_else(|| invalid(format!("BodyLength sai: {}", pretty(len_field))))? as usize;
    let body_at = buf.len();
    buf.resize(body_at + body_len + 7, 0); // body + "10=xxx\x01"
    r.read_exact(&mut buf[body_at..])?;
    verify_checksum(buf)
}

/// Giá trị của lần xuất hiện đầu tiên của tag.
pub fn get(msg: &[u8], tag: u32) -> Option<&[u8]> {
    let mut rest = msg;
    while !rest.is_empty() {
        let eq = rest.iter().position(|&b| b == b'=')?;
        let end = eq + 1 + rest[eq + 1..].iter().position(|&b| b == SOH)?;
        if parse_u64(&rest[..eq]) == Some(tag as u64) {
            return Some(&rest[eq + 1..end]);
        }
        rest = &rest[end + 1..];
    }
    None
}

pub fn get_u64(msg: &[u8], tag: u32) -> Option<u64> {
    get(msg, tag).and_then(parse_u64)
}

pub fn get_str(msg: &[u8], tag: u32) -> &str {
    get(msg, tag).and_then(|v| std::str::from_utf8(v).ok()).unwrap_or("")
}

pub fn msg_type(msg: &[u8]) -> &[u8] {
    get(msg, tag::MSG_TYPE).unwrap_or(b"")
}

fn parse_u64(b: &[u8]) -> Option<u64> {
    if b.is_empty() || b.len() > 19 {
        return None;
    }
    b.iter().try_fold(0u64, |n, &c| c.is_ascii_digit().then(|| n * 10 + (c - b'0') as u64))
}

/// Message với '|' thay SOH, cho log.
pub fn pretty(msg: &[u8]) -> String {
    msg.iter().map(|&b| if b == SOH { '|' } else { b as char }).collect()
}

/// NewOrderSingle mã hoá sẵn: mọi field cố định nằm trong `fixed`, lúc gửi chỉ điền
/// MsgSeqNum, SendingTime và TransactTime.
pub struct OrderTemplate {
    pub rank: usize,
    pub market: Market,
    fixed: Vec<u8>,
    body: Vec<u8>,
}

impl OrderTemplate {
    /// Field theo đúng cách LT-OMS gửi lên sở (xem test sendNew của hnxsim).
    pub fn new(rank: usize, o: &Order, sender_sub_id: Option<&str>, trade_date: &str) -> Self {
        let mut b = Vec::with_capacity(256);
        // field header: phải đứng trước field body đầu tiên
        if let Some(sub) = sender_sub_id {
            put_field(&mut b, tag::SENDER_SUB_ID, sub.as_bytes());
        }
        put_field(&mut b, tag::DELIVER_TO_COMP_ID, o.market.id().as_bytes());
        put_field(&mut b, tag::DELIVER_TO_SUB_ID, o.board.as_bytes());
        // body
        put_field(&mut b, tag::CL_ORD_ID, o.cl_ord_id.as_bytes());
        put_field(&mut b, tag::ACCOUNT, o.account.as_bytes());
        put_field(&mut b, tag::SYMBOL, o.symbol.as_bytes());
        put_field(&mut b, tag::SIDE, &[o.side.wire()]);
        put_field_u64(&mut b, tag::ORDER_QTY, o.qty as u64);
        let (ord_type, tif) = o.kind.wire();
        put_field(&mut b, tag::ORD_TYPE, ord_type);
        if o.kind == OrdKind::Lo {
            put_field_u64(&mut b, tag::PRICE, o.price as u64);
        }
        put_field(&mut b, tag::TIME_IN_FORCE, tif);
        put_field(&mut b, tag::HANDL_INST, b"1");
        put_field(&mut b, tag::TRADE_DATE, trade_date.as_bytes());
        put_field(&mut b, tag::CASH_MARGIN, o.cash_margin.as_bytes());
        put_field(&mut b, tag::ACCOUNT_TYPE, o.account_type.as_bytes());
        put_field(&mut b, tag::INVEST_CODE, o.invest_code.as_bytes());
        put_field(&mut b, tag::FORN_INVEST_TYPE_CODE, o.forn_invest_type_code.as_bytes());
        put_field(&mut b, tag::OPEN_CLOSE, b"1");
        put_field(&mut b, tag::COUNTRY, b"VN");
        let body = Vec::with_capacity(b.len() + 4 + 21 + 1);
        OrderTemplate { rank, market: o.market, fixed: b, body }
    }

    /// Ghi message vào dst (không xoá dst trước).
    pub fn encode(&mut self, dst: &mut Vec<u8>, hdr: &Header, seq: u64, now: &Timestamp) {
        self.body.clear();
        self.body.extend_from_slice(&self.fixed);
        put_field(&mut self.body, tag::TRANSACT_TIME, now);
        hdr.encode(dst, msg::NEW_ORDER, seq, now, &self.body);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::order::Side;
    use std::io::BufReader;

    fn hdr() -> Header {
        Header { begin_string: "FIX.4.4".into(), sender: "HOSE01".into(), target: "LTHOSE".into() }
    }

    #[test]
    fn encode_then_read_roundtrip() {
        let mut out = Vec::new();
        let mut body = Vec::new();
        put_field_u64(&mut body, tag::ENCRYPT_METHOD, 0);
        put_field_u64(&mut body, tag::HEART_BT_INT, 30);
        hdr().encode(&mut out, msg::LOGON, 1, b"20261025-02:00:00.000", &body);
        assert_eq!(
            pretty(&out),
            "8=FIX.4.4|9=67|35=A|34=1|49=HOSE01|56=LTHOSE|52=20261025-02:00:00.000|98=0|108=30|10=070|"
        );
        // hai message liền nhau trên stream
        let mut stream = out.clone();
        stream.extend_from_slice(&out);
        let mut r = BufReader::new(&stream[..]);
        let mut buf = Vec::new();
        for _ in 0..2 {
            read_message(&mut r, &mut buf).unwrap();
            assert_eq!(buf, out);
            assert_eq!(msg_type(&buf), b"A");
            assert_eq!(get_u64(&buf, tag::HEART_BT_INT), Some(30));
        }
        assert_eq!(read_message(&mut r, &mut buf).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    /// Trả dữ liệu từng mẩu `step` byte, xen giữa là WouldBlock như socket non-blocking.
    struct Trickle<'a> {
        data: &'a [u8],
        step: usize,
        block_next: bool,
    }

    impl Read for Trickle<'_> {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            self.block_next = !self.block_next;
            if self.block_next {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            let n = self.step.min(out.len()).min(self.data.len());
            out[..n].copy_from_slice(&self.data[..n]);
            self.data = &self.data[n..];
            Ok(n)
        }
    }

    #[test]
    fn frame_reader_survives_partial_reads() {
        let mut stream = Vec::new();
        let mut want = Vec::new();
        for seq in 1..=50u64 {
            let mut m = Vec::new();
            let body = vec![b'x'; (seq * 7 % 300) as usize];
            let mut b = Vec::new();
            put_field(&mut b, tag::TEXT, &body);
            hdr().encode(&mut m, msg::HEARTBEAT, seq, b"20261025-02:00:00.000", &b);
            stream.extend_from_slice(&m);
            want.push(m);
        }
        for step in [1, 3, 17, 64, 1000] {
            let mut src = Trickle { data: &stream, step, block_next: false };
            let mut fr = FrameReader::new(64); // nhỏ hơn message: phải tự nới
            let mut got = Vec::new();
            loop {
                while let Some(f) = fr.next_frame().unwrap() {
                    got.push(f.to_vec());
                }
                match fr.fill(&mut src) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(e) => panic!("{e}"),
                }
            }
            assert_eq!(got, want, "step {step}");
        }
    }

    #[test]
    fn frame_reader_rejects_garbage() {
        let mut fr = FrameReader::new(64);
        fr.fill(&mut &b"GET / HTTP/1.1\r\n"[..]).unwrap();
        assert_eq!(fr.next_frame().unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn bad_checksum_is_rejected() {
        let mut out = Vec::new();
        hdr().encode(&mut out, msg::HEARTBEAT, 7, b"20261025-02:00:00.000", b"");
        let n = out.len();
        out[n - 2] = if out[n - 2] == b'9' { b'0' } else { b'9' };
        let err = read_message(&mut BufReader::new(&out[..]), &mut Vec::new()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn order_template_fields() {
        let o = Order {
            line: 2,
            received_at: None,
            account: "011C000001".into(),
            market: Market::Sto,
            board: "G1".into(),
            symbol: "VN000000HCM0".into(),
            side: Side::Buy,
            kind: OrdKind::Lo,
            qty: 1000,
            price: 24_200,
            account_type: "1".into(),
            cash_margin: "1".into(),
            invest_code: "1".into(),
            forn_invest_type_code: "00".into(),
            cl_ord_id: "R261025000001".into(),
        };
        let mut t = OrderTemplate::new(0, &o, Some("HSC"), "20261025");
        let mut out = Vec::new();
        t.encode(&mut out, &hdr(), 42, b"20261025-02:00:00.001");
        let mut buf = Vec::new();
        read_message(&mut BufReader::new(&out[..]), &mut buf).unwrap();
        assert_eq!(msg_type(&buf), b"D");
        assert_eq!(get_u64(&buf, tag::MSG_SEQ_NUM), Some(42));
        assert_eq!(get_str(&buf, tag::DELIVER_TO_COMP_ID), "STO");
        assert_eq!(get_str(&buf, tag::CL_ORD_ID), "R261025000001");
        assert_eq!(get_str(&buf, tag::PRICE), "24200");
        assert_eq!(get_str(&buf, tag::ORD_TYPE), "2");
        assert_eq!(get_str(&buf, tag::TRANSACT_TIME), "20261025-02:00:00.001");
        // encode lần hai dùng lại buffer nội bộ, không còn field của lần trước
        out.clear();
        t.encode(&mut out, &hdr(), 43, b"20261025-02:00:00.002");
        read_message(&mut BufReader::new(&out[..]), &mut buf).unwrap();
        assert_eq!(get_str(&buf, tag::TRANSACT_TIME), "20261025-02:00:00.002");
        assert_eq!(buf.windows(4).filter(|w| w == b"\x0160=").count(), 1);
    }
}
