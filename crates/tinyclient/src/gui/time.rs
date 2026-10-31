//! Every date and time on screen, in the machine's time zone (`polish/01-system.md` §2):
//!
//! | Age        | Row ([`stamp`]) | Message ([`clock`]) | Day divider ([`divider`]) | Tooltip ([`tooltip`])                  |
//! |------------|-----------------|---------------------|---------------------------|----------------------------------------|
//! | Today      | `10:37 PM`      | `10:37 PM`          | Today                     | Tuesday, October 13, 2026 at 10:37 PM  |
//! | Yesterday  | Yesterday       | `10:37 PM`          | Yesterday                 | same form                              |
//! | 2–6 days   | `Mon`           | `10:37 PM`          | Monday                    | same form                              |
//! | This year  | `Oct 2`         | `10:37 PM`          | Friday, October 2         | same form                              |
//! | Older      | `10/2/25`       | `10:37 PM`          | October 2, 2025           | same form                              |
//!
//! 12- or 24-hour follows macOS (read once at launch by [`read_clock_preference`]). No relative
//! times: "Last reply 9:24 PM", never "1h ago". Durations are words ([`duration`]). Labels roll
//! over once at midnight: [`until_midnight`] is when to wake for that, never a minute timer.

use std::sync::atomic::{AtomicU8, Ordering};

/// A local moment, broken down. Months and weekdays count from 0 (January, Sunday); years are
/// full (2026).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Local {
    pub year: i32,
    pub day_of_year: i32,
    pub month: usize,
    pub day: i32,
    pub weekday: usize,
    pub hour: i32,
    pub minute: i32,
}

pub fn local(unix: i64) -> Local {
    let seconds = unix as libc::time_t;
    let tm = broken_down_local(seconds);
    Local {
        year: 1900 + tm.tm_year,
        day_of_year: tm.tm_yday,
        month: tm.tm_mon as usize,
        day: tm.tm_mday,
        weekday: tm.tm_wday as usize,
        hour: tm.tm_hour,
        minute: tm.tm_min,
    }
}

pub(super) fn broken_down_local(seconds: libc::time_t) -> libc::tm {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `localtime_r` / `localtime_s` write a full `tm` into the buffer we own.
    unsafe {
        #[cfg(unix)]
        {
            libc::localtime_r(&seconds, &mut tm);
        }
        #[cfg(windows)]
        {
            libc::localtime_s(&mut tm, &seconds);
        }
    }
    tm
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const MONTHS_LONG: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November",
    "December",
];
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const WEEKDAYS_LONG: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

/// How macOS writes the time and a short date, as far as the labels here care.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Style {
    pub hour24: bool,
    /// `10/2/25` (month first) or `2/10/25` (day first).
    pub month_first: bool,
}

// 0: not read yet (12-hour, month first, as in the US); then 1 + hour24 + 2 × day_first.
static STYLE: AtomicU8 = AtomicU8::new(0);

pub fn style() -> Style {
    let bits = STYLE.load(Ordering::Relaxed).saturating_sub(1);
    Style { hour24: bits & 1 != 0, month_first: bits & 2 == 0 }
}

fn set_style(style: Style) {
    STYLE.store(1 + u8::from(style.hour24) + 2 * u8::from(!style.month_first), Ordering::Relaxed);
}

/// The style for a locale such as `en_US` or `de_DE`, with System Settings' 12/24-hour
/// override (`Some(true)` forces 24-hour).
pub fn style_for(locale: &str, force24: Option<bool>) -> Style {
    let region = locale.split(['_', '-', '@']).nth(1).unwrap_or("");
    // Regions whose clocks read 12-hour by default.
    const TWELVE: [&str; 12] = ["US", "CA", "AU", "NZ", "IN", "PH", "PK", "BD", "EG", "SA", "MY", "CO"];
    // Regions that write the month before the day.
    const MONTH_FIRST: [&str; 3] = ["US", "PH", "CA"];
    let twelve = TWELVE.contains(&region) || region.is_empty();
    Style { hour24: force24.unwrap_or(!twelve), month_first: MONTH_FIRST.contains(&region) || region.is_empty() }
}

/// Locale name plus the Windows clock flags. `itime` is `"1"` for 24-hour and `"0"` for
/// 12-hour. `idate` is `"0"` month-first, `"1"` day-first, `"2"` year-first.
pub fn style_from_locale(locale: &str, itime: Option<&str>, idate: Option<&str>) -> Style {
    let force = match itime {
        Some("1") => Some(true),
        Some("0") => Some(false),
        _ => None,
    };
    let mut style = style_for(&locale.replace('-', "_"), force);
    if let Some(idate) = idate {
        style.month_first = idate == "0";
    }
    style
}

/// Reads the machine's clock and region once, off the main thread.
pub fn read_clock_preference() {
    let _ = std::thread::Builder::new().name("clock-style".into()).stack_size(64 * 1024).spawn(|| set_style(platform_clock()));
}

fn platform_clock() -> Style {
    #[cfg(target_os = "macos")]
    {
        let read = |key: &str| {
            std::process::Command::new("/usr/bin/defaults")
                .args(["read", "-g", key])
                .stderr(std::process::Stdio::null())
                .output()
                .ok()
                .filter(|out| out.status.success())
                .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        };
        let locale = read("AppleLocale").unwrap_or_default();
        let force = match (read("AppleICUForce24HourTime").as_deref(), read("AppleICUForce12HourTime").as_deref()) {
            (Some("1"), _) => Some(true),
            (_, Some("1")) => Some(false),
            _ => None,
        };
        return style_for(&locale, force);
    }
    #[cfg(windows)]
    {
        return windows_clock();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let locale = std::env::var("LC_TIME").or_else(|_| std::env::var("LC_ALL")).or_else(|_| std::env::var("LANG")).unwrap_or_default();
        return style_for(&locale, None);
    }
    #[allow(unreachable_code)]
    style_for("", None)
}

#[cfg(windows)]
fn windows_clock() -> Style {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetLocaleInfoW(locale: u32, kind: u32, buf: *mut u16, size: i32) -> i32;
    }
    fn info(kind: u32) -> Option<String> {
        let mut buf = [0u16; 85];
        // SAFETY: the buffer is writable and its length is passed. 0x400 is LOCALE_USER_DEFAULT.
        let count = unsafe { GetLocaleInfoW(0x400, kind, buf.as_mut_ptr(), buf.len() as i32) };
        if count <= 1 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..count as usize - 1]))
    }
    // 0x5c LOCALE_SNAME, 0x23 LOCALE_ITIME, 0x21 LOCALE_IDATE.
    style_from_locale(&info(0x5c).unwrap_or_default(), info(0x23).as_deref(), info(0x21).as_deref())
}

fn clock_in(t: Local, style: Style) -> String {
    if style.hour24 {
        return format!("{:02}:{:02}", t.hour, t.minute);
    }
    let (hour, half) = match t.hour {
        0 => (12, "AM"),
        1..=11 => (t.hour, "AM"),
        12 => (12, "PM"),
        _ => (t.hour - 12, "PM"),
    };
    format!("{hour}:{:02} {half}", t.minute)
}

/// `10:37 PM` or `22:37`: a message's time, whatever its age.
pub fn clock(unix: i64) -> String {
    clock_in(local(unix), style())
}

/// True when both instants fall on the same local calendar day.
pub fn same_day(a: i64, b: i64) -> bool {
    let (a, b) = (local(a), local(b));
    (a.year, a.day_of_year) == (b.year, b.day_of_year)
}

/// How many calendar days `then` is before `today`: 0 today, 1 yesterday. Across a year end too.
fn days_before(today: Local, then: Local) -> i32 {
    if today.year == then.year {
        return today.day_of_year - then.day_of_year;
    }
    let leap = |year: i32| (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let mut days = today.day_of_year;
    let mut year = today.year - 1;
    while year > then.year {
        days += if leap(year) { 366 } else { 365 };
        year -= 1;
    }
    days + (if leap(then.year) { 366 } else { 365 }) - then.day_of_year
}

/// The short stamp on a row, in a style: see the table at the top.
pub fn stamp_in(today: Local, then: Local, style: Style) -> String {
    match days_before(today, then) {
        0 => clock_in(then, style),
        1 => "Yesterday".to_owned(),
        2..=6 => WEEKDAYS[then.weekday % 7].to_owned(),
        _ if then.year == today.year => format!("{} {}", MONTHS[then.month % 12], then.day),
        _ => {
            let (m, d, y) = (then.month + 1, then.day, then.year % 100);
            if style.month_first { format!("{m}/{d}/{y:02}") } else { format!("{d}/{m}/{y:02}") }
        }
    }
}

/// The time on a conversation row.
pub fn stamp(now: i64, unix: i64) -> String {
    stamp_in(local(now), local(unix), style())
}

/// A day divider's words, in the table's form.
pub fn divider_in(today: Local, then: Local) -> String {
    match days_before(today, then) {
        0 => "Today".to_owned(),
        1 => "Yesterday".to_owned(),
        2..=6 => WEEKDAYS_LONG[then.weekday % 7].to_owned(),
        _ if then.year == today.year => {
            format!("{}, {} {}", WEEKDAYS_LONG[then.weekday % 7], MONTHS_LONG[then.month % 12], then.day)
        }
        _ => format!("{} {}, {}", MONTHS_LONG[then.month % 12], then.day, then.year),
    }
}

/// A day divider in the conversation, and the Inbox's day headers.
pub fn divider(now: i64, unix: i64) -> String {
    divider_in(local(now), local(unix))
}

/// Kept for callers: the divider's words.
pub fn day(now: i64, unix: i64) -> String {
    divider(now, unix)
}

/// `Tuesday, October 13, 2026 at 10:37 PM`: the tooltip on every time.
#[allow(dead_code)]
pub fn tooltip_in(then: Local, style: Style) -> String {
    format!(
        "{}, {} {}, {} at {}",
        WEEKDAYS_LONG[then.weekday % 7],
        MONTHS_LONG[then.month % 12],
        then.day,
        then.year,
        clock_in(then, style)
    )
}

#[allow(dead_code)]
pub fn tooltip(unix: i64) -> String {
    tooltip_in(local(unix), style())
}

/// `Mar 21, 2018`: a date on its own (member since, a shared file).
pub fn date(unix: i64) -> String {
    let t = local(unix);
    format!("{} {}, {}", MONTHS[t.month % 12], t.day, t.year)
}

/// A length of time in words: "less than a minute", "1 minute", "14 minutes", "2 hours",
/// "1 hour 5 minutes", "3 days".
#[allow(dead_code)]
pub fn duration(seconds: u64) -> String {
    let plural = |n: u64, one: &str| if n == 1 { format!("1 {one}") } else { format!("{n} {one}s") };
    let minutes = seconds / 60;
    match minutes {
        0 => "less than a minute".to_owned(),
        1..=59 => plural(minutes, "minute"),
        60..=1439 => {
            let (hours, rest) = (minutes / 60, minutes % 60);
            if rest == 0 { plural(hours, "hour") } else { format!("{} {}", plural(hours, "hour"), plural(rest, "minute")) }
        }
        _ => plural(minutes / 1440, "day"),
    }
}

/// Seconds from `now` to the next local midnight: when day labels roll over.
#[allow(dead_code)]
pub fn until_midnight(now: i64) -> u64 {
    let t = local(now);
    let passed = (t.hour * 3600 + t.minute * 60) as i64 + now.rem_euclid(60);
    (86_400 - passed).max(1) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(year: i32, day_of_year: i32, month: usize, day: i32, weekday: usize, hour: i32, minute: i32) -> Local {
        Local { year, day_of_year, month, day, weekday, hour, minute }
    }

    const US: Style = Style { hour24: false, month_first: true };
    const UK: Style = Style { hour24: true, month_first: false };

    // Tuesday, October 13, 2026, 22:37.
    fn today() -> Local {
        at(2026, 285, 9, 13, 2, 22, 37)
    }

    #[test]
    fn a_windows_locale_keeps_its_own_clock_instead_of_a_blank_us_default() {
        assert_eq!(style_from_locale("de-DE", Some("1"), Some("1")), Style { hour24: true, month_first: false });
        assert_eq!(style_from_locale("en-US", Some("0"), Some("0")), US);
        assert_eq!(style_from_locale("en-US", Some("1"), Some("0")), Style { hour24: true, month_first: true });
        assert_eq!(style_from_locale("en-GB", None, None), UK);
    }

    #[test]
    fn a_row_says_the_time_today_then_yesterday_then_the_weekday_then_the_date() {
        assert_eq!(stamp_in(today(), today(), US), "10:37 PM");
        assert_eq!(stamp_in(today(), today(), UK), "22:37");
        assert_eq!(stamp_in(today(), at(2026, 284, 9, 12, 1, 9, 5), US), "Yesterday");
        assert_eq!(stamp_in(today(), at(2026, 280, 9, 7, 3, 9, 5), US), "Wed");
        assert_eq!(stamp_in(today(), at(2026, 274, 9, 2, 5, 9, 5), US), "Oct 2");
        assert_eq!(stamp_in(today(), at(2025, 274, 9, 2, 4, 9, 5), US), "10/2/25");
        assert_eq!(stamp_in(today(), at(2025, 274, 9, 2, 4, 9, 5), UK), "2/10/25");
    }

    #[test]
    fn midnight_noon_and_a_year_end_read_right() {
        assert_eq!(clock_in(at(2026, 0, 0, 1, 4, 0, 5), US), "12:05 AM");
        assert_eq!(clock_in(at(2026, 0, 0, 1, 4, 12, 0), US), "12:00 PM");
        assert_eq!(clock_in(at(2026, 0, 0, 1, 4, 7, 9), UK), "07:09");
        // New Year's Day: December 31 is yesterday, December 28 four days ago.
        let first = at(2027, 0, 0, 1, 5, 9, 0);
        assert_eq!(stamp_in(first, at(2026, 364, 11, 31, 4, 23, 0), US), "Yesterday");
        assert_eq!(stamp_in(first, at(2026, 361, 11, 28, 1, 23, 0), US), "Mon");
        assert_eq!(divider_in(first, at(2026, 364, 11, 31, 4, 23, 0)), "Yesterday");
    }

    #[test]
    fn a_divider_spells_out_the_day_and_a_tooltip_says_everything() {
        assert_eq!(divider_in(today(), today()), "Today");
        assert_eq!(divider_in(today(), at(2026, 280, 9, 7, 3, 1, 0)), "Wednesday");
        assert_eq!(divider_in(today(), at(2026, 274, 9, 2, 5, 1, 0)), "Friday, October 2");
        assert_eq!(divider_in(today(), at(2025, 274, 9, 2, 4, 1, 0)), "October 2, 2025");
        assert_eq!(tooltip_in(today(), US), "Tuesday, October 13, 2026 at 10:37 PM");
        assert_eq!(tooltip_in(today(), UK), "Tuesday, October 13, 2026 at 22:37");
    }

    #[test]
    fn the_clock_follows_the_region_and_the_override() {
        assert_eq!(style_for("en_US", None), US);
        assert_eq!(style_for("en_GB", None), UK);
        assert_eq!(style_for("de_DE", None), UK);
        assert_eq!(style_for("en_US", Some(true)), Style { hour24: true, month_first: true });
        assert_eq!(style_for("en_GB", Some(false)), Style { hour24: false, month_first: false });
        set_style(UK);
        assert_eq!(style(), UK);
        set_style(US);
        assert_eq!(style(), US);
    }

    #[test]
    fn durations_are_words() {
        assert_eq!(duration(30), "less than a minute");
        assert_eq!(duration(60), "1 minute");
        assert_eq!(duration(14 * 60 + 5), "14 minutes");
        assert_eq!(duration(3600), "1 hour");
        assert_eq!(duration(3900), "1 hour 5 minutes");
        assert_eq!(duration(7200), "2 hours");
        assert_eq!(duration(3 * 86_400), "3 days");
    }
}
