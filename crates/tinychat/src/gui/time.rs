//! Wall-clock labels in the machine's time zone.

struct Local {
    year: i32,
    day_of_year: i32,
    month: usize,
    day: i32,
    weekday: usize,
    hour: i32,
    minute: i32,
}

fn local(unix: i64) -> Local {
    let seconds = unix as libc::time_t;
    let tm = broken_down_local(seconds);
    Local {
        year: tm.tm_year,
        day_of_year: tm.tm_yday,
        month: tm.tm_mon as usize,
        day: tm.tm_mday,
        weekday: tm.tm_wday as usize,
        hour: tm.tm_hour,
        minute: tm.tm_min,
    }
}

fn broken_down_local(seconds: libc::time_t) -> libc::tm {
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

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// `2:41 PM`
pub fn clock(unix: i64) -> String {
    let t = local(unix);
    let (hour, half) = match t.hour {
        0 => (12, "AM"),
        1..=11 => (t.hour, "AM"),
        12 => (12, "PM"),
        _ => (t.hour - 12, "PM"),
    };
    format!("{hour}:{:02} {half}", t.minute)
}

/// True when both instants fall on the same local calendar day.
pub fn same_day(a: i64, b: i64) -> bool {
    let (a, b) = (local(a), local(b));
    (a.year, a.day_of_year) == (b.year, b.day_of_year)
}

/// `Mar 21, 2018`
pub fn date(unix: i64) -> String {
    let t = local(unix);
    format!("{} {}, {}", MONTHS[t.month % 12], t.day, t.year)
}

/// `Today`, `Yesterday`, or `Mon, Oct 12`
pub fn day(now: i64, unix: i64) -> String {
    if same_day(now, unix) {
        "Today".to_owned()
    } else if same_day(now - 86_400, unix) {
        "Yesterday".to_owned()
    } else {
        let t = local(unix);
        format!("{}, {} {}", WEEKDAYS[t.weekday % 7], MONTHS[t.month % 12], t.day)
    }
}

/// The short stamp on a conversation row: a clock today, a day before that.
pub fn stamp(now: i64, unix: i64) -> String {
    if same_day(now, unix) {
        clock(unix)
    } else if same_day(now - 86_400, unix) {
        "Yesterday".to_owned()
    } else {
        let t = local(unix);
        format!("{} {}", MONTHS[t.month % 12], t.day)
    }
}
