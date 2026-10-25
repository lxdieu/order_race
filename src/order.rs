//! Lệnh chờ đua: đọc từ CSV và kiểm tra theo luật sàn trước giờ mở cửa, để lệnh sai bị loại
//! ở racer thay vì chiếm chỗ trong hàng gửi.

use std::collections::HashMap;

use crate::clock;
use crate::market::Market;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn wire(self) -> u8 {
        match self {
            Side::Buy => b'1',
            Side::Sell => b'2',
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrdKind {
    Lo,
    Ato,
}

impl OrdKind {
    /// (OrdType 40, TimeInForce 59) theo cách LT-OMS gửi.
    pub fn wire(self) -> (&'static [u8], &'static [u8]) {
        match self {
            OrdKind::Lo => (b"2", b"0"),
            OrdKind::Ato => (b"1", b"2"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Order {
    pub line: usize,
    /// Thời điểm công ty nhận lệnh của khách (unix nano): căn cứ xếp thứ tự gửi.
    pub received_at: Option<i64>,
    pub account: String,
    pub market: Market,
    pub board: String,
    /// Mã ISIN, đúng như gửi lên sở (vd VN000000HCM0).
    pub symbol: String,
    pub side: Side,
    pub kind: OrdKind,
    pub qty: i64,
    pub price: i64,
    pub account_type: String,
    pub cash_margin: String,
    pub invest_code: String,
    pub forn_invest_type_code: String,
    pub cl_ord_id: String,
}

const REQUIRED: [&str; 7] = ["account", "exchange", "symbol", "side", "ord_type", "qty", "price"];

/// Đọc CSV có dòng tiêu đề. Cột bắt buộc: account, exchange (HOSE/HNX/UPCOM hoặc STO/STX/UPX),
/// symbol (ISIN), side (B/S), ord_type (LO/ATO), qty, price. Cột tuỳ chọn: received_at (RFC 3339),
/// board_id (mặc định G1), account_type, cash_margin, invest_code (mặc định 1), forn_invest_type_code
/// (mặc định 00). Dòng trống và dòng bắt đầu bằng '#' bị bỏ qua. Không hỗ trợ field có dấu nháy.
pub fn load_csv(text: &str) -> (Vec<Order>, Vec<String>) {
    let mut orders = Vec::new();
    let mut errors = Vec::new();
    let mut lines = text
        .lines()
        .enumerate()
        .map(|(i, l)| (i + 1, l.trim()))
        .filter(|(_, l)| !l.is_empty() && !l.starts_with('#'));

    let Some((_, header)) = lines.next() else {
        return (orders, vec!["file lệnh rỗng".into()]);
    };
    let cols: HashMap<String, usize> = header
        .trim_start_matches('\u{feff}')
        .split(',')
        .enumerate()
        .map(|(i, c)| (c.trim().to_ascii_lowercase(), i))
        .collect();
    let missing: Vec<_> = REQUIRED.iter().filter(|c| !cols.contains_key(**c)).collect();
    if !missing.is_empty() {
        return (orders, vec![format!("thiếu cột {missing:?}")]);
    }

    for (line, text) in lines {
        match parse_line(line, text, &cols) {
            Ok(o) => orders.push(o),
            Err(e) => errors.push(format!("dòng {line}: {e}")),
        }
    }
    (orders, errors)
}

fn parse_line(line: usize, text: &str, cols: &HashMap<String, usize>) -> Result<Order, String> {
    if text.contains('"') {
        return Err("không hỗ trợ field có dấu nháy".into());
    }
    let fields: Vec<&str> = text.split(',').map(str::trim).collect();
    let get = |name: &str| cols.get(name).and_then(|&i| fields.get(i)).copied().unwrap_or("");
    let or = |name: &str, default: &str| {
        let v = get(name);
        if v.is_empty() { default.to_string() } else { v.to_string() }
    };

    let market = Market::parse(get("exchange")).ok_or_else(|| format!("sàn không hợp lệ {:?}", get("exchange")))?;
    let side = match get("side").to_ascii_uppercase().as_str() {
        "B" | "BUY" | "1" => Side::Buy,
        "S" | "SELL" | "2" => Side::Sell,
        other => return Err(format!("side không hợp lệ {other:?}")),
    };
    let kind = match get("ord_type").to_ascii_uppercase().as_str() {
        "LO" => OrdKind::Lo,
        "ATO" => OrdKind::Ato,
        other => return Err(format!("loại lệnh không hỗ trợ {other:?} (LO, ATO)")),
    };
    let qty: i64 = get("qty").parse().map_err(|_| format!("qty không hợp lệ {:?}", get("qty")))?;
    let price: i64 = match get("price") {
        "" => 0,
        p => p.parse().map_err(|_| format!("price không hợp lệ {p:?} (đơn vị đồng, số nguyên)"))?,
    };
    let received_at = match get("received_at") {
        "" => None,
        t => Some(clock::parse_rfc3339(t)?),
    };

    Ok(Order {
        line,
        received_at,
        account: get("account").to_string(),
        market,
        board: or("board_id", "G1"),
        symbol: get("symbol").to_string(),
        side,
        kind,
        qty,
        price,
        account_type: or("account_type", "1"),
        cash_margin: or("cash_margin", "1"),
        invest_code: or("invest_code", "1"),
        forn_invest_type_code: or("forn_invest_type_code", "00"),
        cl_ord_id: String::new(),
    })
}

pub const LOT_SIZE: i64 = 100;
pub const HOSE_MAX_QTY: i64 = 500_000;

/// Bước giá cổ phiếu theo quy chế KRX. ETF/CW có bước giá riêng: tắt RACE_CHECK_TICK nếu đua các mã đó.
pub fn tick_size(market: Market, price: i64) -> i64 {
    match market {
        Market::Sto if price < 10_000 => 10,
        Market::Sto if price < 50_000 => 50,
        _ => 100,
    }
}

/// Kiểm tra những gì biết được trước giờ mở cửa. Trần/sàn cần bảng giá trong ngày nên chưa kiểm ở đây.
pub fn validate(o: &Order, check_tick: bool) -> Result<(), String> {
    if o.account.is_empty() || o.symbol.is_empty() {
        return Err("thiếu account hoặc symbol".into());
    }
    if o.qty <= 0 {
        return Err(format!("khối lượng {} phải > 0", o.qty));
    }
    if o.qty >= LOT_SIZE && o.qty % LOT_SIZE != 0 {
        return Err(format!("khối lượng {} phải là bội số của {LOT_SIZE} (lô lẻ 1-99 tách lệnh riêng)", o.qty));
    }
    if o.market == Market::Sto && o.qty > HOSE_MAX_QTY {
        return Err(format!("khối lượng {} vượt tối đa {HOSE_MAX_QTY} của HOSE", o.qty));
    }
    match o.kind {
        OrdKind::Lo => {
            if o.price <= 0 {
                return Err("lệnh LO phải có giá".into());
            }
            let tick = tick_size(o.market, o.price);
            if check_tick && o.price % tick != 0 {
                return Err(format!("giá {} sai bước giá {tick}", o.price));
            }
        }
        OrdKind::Ato => {
            if o.market != Market::Sto {
                return Err(format!("lệnh ATO chỉ có ở HOSE, không có ở {}", o.market.exchange()));
            }
            if o.price != 0 {
                return Err("lệnh ATO không có giá (để trống price)".into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CSV: &str = "\
received_at,account,exchange,symbol,side,ord_type,qty,price
# lệnh mẫu
2026-10-24T20:00:00.000+07:00,011C000001,HOSE,VN000000HCM0,B,LO,1000,24200
2026-10-24T20:00:01.000+07:00,011C000002,hnx,VN000000SHS6,S,LO,500,10000

,011C000003,STO,VN000000HCM0,B,ATO,100,
2026-10-24T20:00:02+07:00,011C000004,HOSE,VN000000HCM0,X,LO,100,24200
";

    #[test]
    fn load_and_parse() {
        let (orders, errors) = load_csv(CSV);
        assert_eq!(orders.len(), 3);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].starts_with("dòng 7:"), "{errors:?}");
        assert_eq!(orders[1].market, Market::Stx);
        assert_eq!(orders[1].side, Side::Sell);
        assert_eq!(orders[1].board, "G1");
        assert_eq!(orders[2].kind, OrdKind::Ato);
        assert_eq!(orders[2].received_at, None);
        assert!(orders[0].received_at.unwrap() < orders[1].received_at.unwrap());
    }

    #[test]
    fn missing_column() {
        let (_, errors) = load_csv("account,exchange\nA,HOSE\n");
        assert!(errors[0].contains("thiếu cột"));
    }

    #[test]
    fn validation_rules() {
        let (orders, _) = load_csv(CSV);
        let mut o = orders[0].clone();
        assert!(validate(&o, true).is_ok());
        o.price = 24_210; // HOSE 10.000–49.950: bước 50
        assert!(validate(&o, true).is_err());
        assert!(validate(&o, false).is_ok());
        o.price = 9_990; // dưới 10.000: bước 10
        assert!(validate(&o, true).is_ok());
        o.qty = 150;
        assert!(validate(&o, true).is_err());
        o.qty = 99; // lô lẻ
        assert!(validate(&o, true).is_ok());
        o.qty = 600_000;
        assert!(validate(&o, true).is_err());

        let mut ato = orders[2].clone();
        assert!(validate(&ato, true).is_ok());
        ato.market = Market::Stx;
        assert!(validate(&ato, true).is_err());
    }

    #[test]
    fn ticks() {
        assert_eq!(tick_size(Market::Sto, 9_990), 10);
        assert_eq!(tick_size(Market::Sto, 10_000), 50);
        assert_eq!(tick_size(Market::Sto, 50_000), 100);
        assert_eq!(tick_size(Market::Stx, 9_000), 100);
        assert_eq!(tick_size(Market::Upx, 120_000), 100);
    }
}
