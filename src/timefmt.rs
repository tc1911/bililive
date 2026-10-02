//! 时间。纯函数为主，方便单测；只有 `hhmm` 需要问系统要时区。

use std::time::{SystemTime, UNIX_EPOCH};

/// 北京时间相对 UTC 的偏移。B 站所有时间字段（live_time、弹幕历史）都是这个时区的墙上时间，
/// 而且**不带时区后缀**，只能靠这个常量把它还原成真正的时间戳。
pub const BEIJING_OFFSET: i64 = 8 * 3600;

pub fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `SystemTime` -> 本地时区的 `HH:MM`，弹幕时间和「最后刷新时间」都用它。
///
/// std 里没有本地时区（要自己解 TZif），`libc::localtime_r` 是最短的一条路。
/// 用 `_r` 而不是 `localtime`：后者返回进程内共享的静态缓冲区，tokio 是多线程运行时，
/// 两个线程同时格式化会读到对方的 tm。
pub fn hhmm(t: SystemTime) -> String {
    let Ok(d) = t.duration_since(UNIX_EPOCH) else {
        return "--:--".to_string();
    };
    local_hhmm(d.as_secs() as i64)
}

fn local_hhmm(epoch: i64) -> String {
    // SAFETY: localtime_r 只写我们自己的 tm，且 tm 已清零；失败时返回空指针，下面判了。
    unsafe {
        let secs = epoch as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&secs, &mut tm).is_null() {
            return "--:--".to_string();
        }
        format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
    }
}

/// 秒数 -> 「1天2时3分」。负数按 0 算。
pub fn human_duration(secs: i64) -> String {
    let secs = secs.max(0);
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let minutes = (secs % 3600) / 60;
    if days > 0 {
        format!("{days}天{hours}时{minutes}分")
    } else if hours > 0 {
        format!("{hours}时{minutes}分")
    } else {
        format!("{minutes}分")
    }
}

/// 「已播多久」。`live_time` 是北京时间墙上时间，`now` 是真正的时间戳。
///
/// Go 版是「按 UTC 解析出来再往回补 8 小时」，跟这里的结果一样，但两处偏移很容易抵消错。
/// 这里直接先把墙上时间还原成真时间戳，减出来就是答案。
pub fn live_duration(live_time: &str, now: i64) -> String {
    match beijing_epoch(live_time) {
        Some(start) => human_duration(now - start),
        None => String::new(),
    }
}

/// 「北京时间的墙上时间」-> 真正的时间戳。
pub fn beijing_epoch(wall: &str) -> Option<i64> {
    parse_datetime_utc(wall).map(|s| s - BEIJING_OFFSET)
}

/// `"YYYY-MM-DD HH:MM:SS"` 当成 UTC 解析出来的秒数。
///
/// 月份 0 或日期 0 一律返回 None —— 没开播时接口给的就是 `"0000-00-00 00:00:00"`，
/// 当零值收下再拿去相减，界面会显示十几万天（Go 版真出现过「739891天」）。
pub fn parse_datetime_utc(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, time) = s.split_once(' ')?;
    let mut d = date.split('-');
    let y: i64 = d.next()?.parse().ok()?;
    let m: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;
    if d.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&day) {
        return None;
    }

    let mut t = time.split(':');
    let hh: i64 = t.next()?.parse().ok()?;
    let mm: i64 = t.next()?.parse().ok()?;
    // 秒可以缺（有些接口只到分钟）
    let ss: i64 = t.next().unwrap_or("0").parse().ok()?;
    if !(0..=23).contains(&hh) || !(0..=59).contains(&mm) || !(0..=60).contains(&ss) {
        return None;
    }

    Some(days_from_civil(y, m, day) * 86400 + hh * 3600 + mm * 60 + ss)
}

/// 公历日期 -> 1970-01-01 起的天数（Howard Hinnant 的算法，纯整数，没有时区没有闰秒）。
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (m + 9) % 12; // 三月当 0
    let doy = (153 * mp + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datetime_parses_like_go() {
        assert_eq!(parse_datetime_utc("1970-01-01 00:00:00"), Some(0));
        assert_eq!(parse_datetime_utc("2000-01-01 00:00:00"), Some(946_684_800));
        // 闰日：2020-01-01 起第 59 天
        assert_eq!(
            parse_datetime_utc("2020-02-29 12:00:00"),
            Some(1_582_977_600)
        );
    }

    // 没开播时接口给的就是这个，必须解析失败，否则界面上会出现十几万天。
    #[test]
    fn zero_live_time_is_rejected() {
        assert_eq!(parse_datetime_utc("0000-00-00 00:00:00"), None);
        assert_eq!(live_duration("0000-00-00 00:00:00", 1_700_000_000), "");
        assert_eq!(live_duration("", 1_700_000_000), "");
    }

    #[test]
    fn live_duration_counts_beijing_wall_clock() {
        // 02:00 开播、04:00 现在（都是北京时间）-> 已播 2 小时
        let now = beijing_epoch("2026-10-03 04:00:00").unwrap();
        assert_eq!(live_duration("2026-10-03 02:00:00", now), "2时0分");
    }

    #[test]
    fn human_duration_shapes() {
        assert_eq!(human_duration(0), "0分");
        assert_eq!(human_duration(59), "0分");
        assert_eq!(human_duration(60), "1分");
        assert_eq!(human_duration(3 * 3600 + 5 * 60), "3时5分");
        assert_eq!(human_duration(86400 + 2 * 3600 + 3 * 60), "1天2时3分");
        // 时钟回拨或者主播时间比我们快时不能出现负数
        assert_eq!(human_duration(-100), "0分");
    }
}
