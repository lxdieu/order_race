//! Đồng hồ: giờ hệ thống (unix nano), định dạng UTCTimestamp của FIX, đọc giờ VN và duration.
//! Giờ sở là giờ VN, độ lệch cố định (không đổi giờ mùa hè) nên không cần thư viện múi giờ.

use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const NANOS_PER_SEC: i64 = 1_000_000_000;
const SECS_PER_DAY: i64 = 86_400;

pub fn now_nanos() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

/// FIX UTCTimestamp "YYYYMMDD-HH:MM:SS.sss".
pub type Timestamp = [u8; 21];

pub fn utc_timestamp(unix_nanos: i64) -> Timestamp {
    let secs = unix_nanos.div_euclid(NANOS_PER_SEC);
    let ms = (unix_nanos.rem_euclid(NANOS_PER_SEC) / 1_000_000) as u32;
    let (y, mo, d) = civil_from_days(secs.div_euclid(SECS_PER_DAY));
    let sod = secs.rem_euclid(SECS_PER_DAY) as u32;
    let mut out = *b"00000000-00:00:00.000";
    put_digits(&mut out[0..4], y as u32);
    put_digits(&mut out[4..6], mo);
    put_digits(&mut out[6..8], d);
    put_digits(&mut out[9..11], sod / 3600);
    put_digits(&mut out[12..14], sod / 60 % 60);
    put_digits(&mut out[15..17], sod % 60);
    put_digits(&mut out[18..21], ms);
    out
}

fn put_digits(dst: &mut [u8], mut v: u32) {
    for b in dst.iter_mut().rev() {
        *b = b'0' + (v % 10) as u8;
        v /= 10;
    }
}

/// Ngày theo lịch từ số ngày kể từ 1970-01-01 (thuật toán của Howard Hinnant).
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = yoe + era * 400;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let (m, d) = (m as i64, d as i64);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Ngày địa phương (y, m, d) của một thời điểm, với độ lệch UTC tính bằng giây.
pub fn local_date(unix_nanos: i64, offset_secs: i32) -> (i64, u32, u32) {
    let secs = unix_nanos.div_euclid(NANOS_PER_SEC) + offset_secs as i64;
    civil_from_days(secs.div_euclid(SECS_PER_DAY))
}

/// "YYYYMMDD" theo giờ địa phương (TradeDate, tên file journal).
pub fn local_yyyymmdd(unix_nanos: i64, offset_secs: i32) -> String {
    let (y, m, d) = local_date(unix_nanos, offset_secs);
    format!("{y:04}{m:02}{d:02}")
}

/// "HH:MM:SS.ffffff" theo giờ địa phương, cho log.
pub fn local_time_us(unix_nanos: i64, offset_secs: i32) -> String {
    let local = unix_nanos + offset_secs as i64 * NANOS_PER_SEC;
    let sod = local.div_euclid(NANOS_PER_SEC).rem_euclid(SECS_PER_DAY);
    let us = local.rem_euclid(NANOS_PER_SEC) / 1000;
    format!("{:02}:{:02}:{:02}.{:06}", sod / 3600, sod / 60 % 60, sod % 60, us)
}

/// "+07:00", "-05:30", "Z".
pub fn parse_offset(s: &str) -> Result<i32, String> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("z") {
        return Ok(0);
    }
    let (sign, rest) = match s.as_bytes().first() {
        Some(b'+') => (1, &s[1..]),
        Some(b'-') => (-1, &s[1..]),
        _ => return Err(format!("độ lệch UTC không hợp lệ {s:?} (vd +07:00)")),
    };
    let (h, m) = rest.split_once(':').unwrap_or((rest, "0"));
    let h: i32 = h.parse().map_err(|_| format!("độ lệch UTC không hợp lệ {s:?}"))?;
    let m: i32 = m.parse().map_err(|_| format!("độ lệch UTC không hợp lệ {s:?}"))?;
    if h > 14 || m > 59 {
        return Err(format!("độ lệch UTC không hợp lệ {s:?}"));
    }
    Ok(sign * (h * 3600 + m * 60))
}

/// "HH:MM:SS[.fff]" thành số nano từ 00:00.
fn parse_hms(s: &str) -> Option<i64> {
    let mut it = s.split(':');
    let h: i64 = it.next()?.parse().ok()?;
    let m: i64 = it.next()?.parse().ok()?;
    let sec = it.next()?;
    if it.next().is_some() || h > 23 || m > 59 {
        return None;
    }
    let (whole, frac) = sec.split_once('.').unwrap_or((sec, ""));
    let whole: i64 = whole.parse().ok()?;
    if whole > 60 || frac.len() > 9 || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let frac_nanos = if frac.is_empty() {
        0
    } else {
        frac.parse::<i64>().ok()? * 10_i64.pow(9 - frac.len() as u32)
    };
    Some(((h * 60 + m) * 60 + whole) * NANOS_PER_SEC + frac_nanos)
}

/// RFC 3339: "2026-10-25T09:00:00.123+07:00" hoặc "...Z". Dấu cách thay cho 'T' cũng được.
pub fn parse_rfc3339(s: &str) -> Result<i64, String> {
    let bad = || format!("thời điểm không hợp lệ {s:?} (RFC 3339, vd 2026-10-25T08:59:01.5+07:00)");
    let s = s.trim();
    if s.len() < 20 {
        return Err(bad());
    }
    let (date, rest) = s.split_at(10);
    let rest = rest.strip_prefix(['T', 't', ' ']).ok_or_else(bad)?;
    let mut d = date.split('-');
    let y: i64 = d.next().and_then(|v| v.parse().ok()).ok_or_else(bad)?;
    let mo: u32 = d.next().and_then(|v| v.parse().ok()).ok_or_else(bad)?;
    let day: u32 = d.next().and_then(|v| v.parse().ok()).ok_or_else(bad)?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&day) {
        return Err(bad());
    }
    let tz_at = rest.rfind(['Z', 'z', '+', '-']).ok_or_else(bad)?;
    let time = parse_hms(&rest[..tz_at]).ok_or_else(bad)?;
    let offset = parse_offset(&rest[tz_at..])?;
    Ok(days_from_civil(y, mo, day) * SECS_PER_DAY * NANOS_PER_SEC + time - offset as i64 * NANOS_PER_SEC)
}

/// Giờ mở cửa dự kiến: "09:00:00[.fff]" (hôm nay, giờ địa phương), RFC 3339, hoặc "+90s" (tính từ now).
pub fn parse_open_time(s: &str, now: i64, offset_secs: i32) -> Result<i64, String> {
    let s = s.trim();
    if let Some(rel) = s.strip_prefix('+') {
        return Ok(now + parse_duration(rel)?.as_nanos() as i64);
    }
    if let Some(tod) = parse_hms(s) {
        let (y, m, d) = local_date(now, offset_secs);
        let midnight = days_from_civil(y, m, d) * SECS_PER_DAY * NANOS_PER_SEC;
        return Ok(midnight + tod - offset_secs as i64 * NANOS_PER_SEC);
    }
    parse_rfc3339(s).map_err(|_| format!("giờ không hợp lệ {s:?}: cần HH:MM:SS[.fff], RFC 3339 hoặc +duration"))
}

/// Duration dạng Go: "500ms", "1.5s", "2m", "1m30s", "250us". Đơn vị: ns, us, µs, ms, s, m, h.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let bad = || format!("duration không hợp lệ {s:?} (vd 500ms, 2s, 1m30s)");
    let mut rest = s.trim();
    if rest.is_empty() {
        return Err(bad());
    }
    if rest == "0" {
        return Ok(Duration::ZERO);
    }
    let mut total = 0f64;
    while !rest.is_empty() {
        let num_len = rest.find(|c: char| !(c.is_ascii_digit() || c == '.')).ok_or_else(bad)?;
        if num_len == 0 {
            return Err(bad());
        }
        let value: f64 = rest[..num_len].parse().map_err(|_| bad())?;
        rest = &rest[num_len..];
        let unit_len = rest.find(|c: char| c.is_ascii_digit() || c == '.').unwrap_or(rest.len());
        let scale = match &rest[..unit_len] {
            "ns" => 1.0,
            "us" | "µs" => 1e3,
            "ms" => 1e6,
            "s" => 1e9,
            "m" => 60e9,
            "h" => 3600e9,
            _ => return Err(bad()),
        };
        total += value * scale;
        rest = &rest[unit_len..];
    }
    Ok(Duration::from_nanos(total.round() as u64))
}

/// Ngủ tới thời điểm hệ thống `target` (unix nano). Ngủ từng đoạn tối đa 1s để bám theo
/// đồng hồ hệ thống nếu nó bị chỉnh (NTP).
pub fn sleep_until(target: i64) {
    loop {
        let left = target - now_nanos();
        if left <= 0 {
            return;
        }
        thread::sleep(Duration::from_nanos(left.min(NANOS_PER_SEC) as u64));
    }
}

/// Instant tương ứng với thời điểm hệ thống `target` (quá khứ thì là bây giờ).
pub fn instant_at(target: i64) -> Instant {
    let left = target - now_nanos();
    Instant::now() + Duration::from_nanos(left.max(0) as u64)
}

/// Hiển thị duration gọn: 850ns, 35.2µs, 1.25ms, 2.300s.
pub fn fmt_dur(d: Duration) -> String {
    let n = d.as_nanos();
    if n < 1_000 {
        format!("{n}ns")
    } else if n < 1_000_000 {
        format!("{:.1}µs", n as f64 / 1e3)
    } else if n < 1_000_000_000 {
        format!("{:.2}ms", n as f64 / 1e6)
    } else {
        format!("{:.3}s", n as f64 / 1e9)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VN: i32 = 7 * 3600;

    #[test]
    fn timestamp_format() {
        // 2026-10-25T02:00:00.123Z
        let t = parse_rfc3339("2026-10-25T09:00:00.123+07:00").unwrap();
        assert_eq!(&utc_timestamp(t), b"20261025-02:00:00.123");
    }

    #[test]
    fn civil_roundtrip() {
        for days in [-1000, 0, 1, 59, 60, 365, 10_957, 20_000, 20_751] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days, "{y}-{m}-{d}");
        }
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 2, 29));
    }

    #[test]
    fn open_time_formats() {
        let now = parse_rfc3339("2026-10-25T08:30:00+07:00").unwrap();
        let want = parse_rfc3339("2026-10-25T09:00:00+07:00").unwrap();
        assert_eq!(parse_open_time("09:00:00", now, VN).unwrap(), want);
        assert_eq!(parse_open_time("09:00:00.250", now, VN).unwrap(), want + 250_000_000);
        assert_eq!(parse_open_time("+30m", now, VN).unwrap(), want);
        assert_eq!(parse_open_time("2026-10-25T02:00:00Z", now, VN).unwrap(), want);
        assert!(parse_open_time("9h", now, VN).is_err());
        // 23:30 giờ UTC đã là ngày hôm sau ở VN
        let late = parse_rfc3339("2026-10-25T23:30:00Z").unwrap();
        assert_eq!(local_yyyymmdd(late, VN), "20261026");
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("1m30s").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("1.5s").unwrap(), Duration::from_millis(1500));
        assert_eq!(parse_duration("250us").unwrap(), Duration::from_micros(250));
        assert_eq!(parse_duration("0").unwrap(), Duration::ZERO);
        assert!(parse_duration("5").is_err());
        assert!(parse_duration("5x").is_err());
    }

    #[test]
    fn local_time_format() {
        let t = parse_rfc3339("2026-10-25T09:00:00.000123+07:00").unwrap();
        assert_eq!(local_time_us(t, VN), "09:00:00.000123");
    }
}
