//! The words on a day divider. `gui::time` has the short forms the sidebar needs; a divider
//! spells the day out.

const MONTHS: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November",
    "December",
];
const WEEKDAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

/// A local calendar day: year, day of the year, month from 0, day of the month, weekday from
/// Sunday.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Day {
    pub year: i32,
    pub of_year: i32,
    pub month: usize,
    pub of_month: i32,
    pub weekday: usize,
}

pub fn local_day(unix: i64) -> Day {
    let seconds = unix as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `localtime_r` / `localtime_s` write a full `tm` into the buffer we own.
    unsafe {
        #[cfg(unix)]
        libc::localtime_r(&seconds, &mut tm);
        #[cfg(windows)]
        libc::localtime_s(&mut tm, &seconds);
    }
    Day { year: tm.tm_year, of_year: tm.tm_yday, month: tm.tm_mon as usize, of_month: tm.tm_mday, weekday: tm.tm_wday as usize }
}

/// `Today`, `Yesterday`, or `Monday, October 12`. The year is added once it is not this one.
pub fn day_words(today: Day, yesterday: Day, day: Day) -> String {
    let same = |a: Day, b: Day| (a.year, a.of_year) == (b.year, b.of_year);
    if same(today, day) {
        return "Today".to_owned();
    }
    if same(yesterday, day) {
        return "Yesterday".to_owned();
    }
    let words = format!("{}, {} {}", WEEKDAYS[day.weekday % 7], MONTHS[day.month % 12], day.of_month);
    if day.year == today.year {
        words
    } else {
        format!("{words}, {}", 1900 + day.year)
    }
}

pub fn day_label(now: i64, unix: i64) -> String {
    day_words(local_day(now), local_day(now - 86_400), local_day(unix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(year: i32, of_year: i32, month: usize, of_month: i32, weekday: usize) -> Day {
        Day { year, of_year, month, of_month, weekday }
    }

    #[test]
    fn a_day_divider_spells_out_the_weekday_and_the_month() {
        let today = day(126, 285, 9, 13, 2);
        let yesterday = day(126, 284, 9, 12, 1);
        assert_eq!(day_words(today, yesterday, today), "Today");
        assert_eq!(day_words(today, yesterday, yesterday), "Yesterday");
        assert_eq!(day_words(today, yesterday, day(126, 283, 9, 11, 0)), "Sunday, October 11");
        assert_eq!(day_words(today, yesterday, day(126, 62, 2, 4, 3)), "Wednesday, March 4");
        // The same date a year earlier is not today, and says which year.
        assert_eq!(day_words(today, yesterday, day(125, 285, 9, 13, 1)), "Monday, October 13, 2025");
        // New Year's Day: yesterday is in the year before.
        let first = day(127, 0, 0, 1, 5);
        let eve = day(126, 364, 11, 31, 4);
        assert_eq!(day_words(first, eve, eve), "Yesterday");
    }
}
