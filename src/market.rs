//! Thị trường theo mã KRX (tag 128 MarketID của lệnh, tag 115 của K04).

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Market {
    Sto, // HOSE
    Stx, // HNX niêm yết
    Upx, // UPCoM
}

impl Market {
    pub const ALL: [Market; 3] = [Market::Sto, Market::Stx, Market::Upx];

    pub fn index(self) -> usize {
        self as usize
    }

    /// Bit trong cờ kích hoạt của Trigger.
    pub fn bit(self) -> u32 {
        1 << self.index()
    }

    pub fn id(self) -> &'static str {
        match self {
            Market::Sto => "STO",
            Market::Stx => "STX",
            Market::Upx => "UPX",
        }
    }

    pub fn exchange(self) -> &'static str {
        match self {
            Market::Sto => "HOSE",
            Market::Stx => "HNX",
            Market::Upx => "UPCOM",
        }
    }

    /// Nhận cả mã KRX (STO/STX/UPX) lẫn tên sàn (HOSE/HNX/UPCOM), không phân biệt hoa thường.
    pub fn parse(s: &str) -> Option<Market> {
        match s.trim().to_ascii_uppercase().as_str() {
            "STO" | "HOSE" => Some(Market::Sto),
            "STX" | "HNX" => Some(Market::Stx),
            "UPX" | "UPCOM" => Some(Market::Upx),
            _ => None,
        }
    }

    /// Đọc đúng mã KRX trên wire (K04 tag 115).
    pub fn from_wire(b: &[u8]) -> Option<Market> {
        match b {
            b"STO" => Some(Market::Sto),
            b"STX" => Some(Market::Stx),
            b"UPX" => Some(Market::Upx),
            _ => None,
        }
    }
}
