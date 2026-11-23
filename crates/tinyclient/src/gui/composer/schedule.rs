//! Times the send menu can hand a network. The network sends the message then.
//! tinyclient does not run a timer, and it does not list, edit, or cancel: no request does.
//!
//! A network that does not set `Capabilities::schedule` is not offered a time. Putting one on
//! the send anyway is refused, so the message does not go out now by mistake.

/// One choice in the send menu: the words, and the unix seconds the send carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moment {
    pub label: String,
    pub at: i64,
}

/// A local calendar day the hour list is for. Months count from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Day {
    pub year: i32,
    pub month: i32,
    pub day: i32,
}

const WEEKDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

/// Right-click on Send opens when the network can do something other than send now.
pub fn send_menu_for(silent: bool, schedule: bool) -> bool {
    silent || schedule
}

/// Whether "Send when online" is offered in this conversation: Telegram 1:1 DMs only,
/// not Saved Messages, and only when the recipient's last-seen time is visible.
pub fn when_online_for(network: protocol::Network, kind: protocol::ConversationKind, is_self: bool, hidden_last_seen: bool) -> bool {
    network == protocol::Network::Telegram && kind == protocol::ConversationKind::Direct && !is_self && !hidden_last_seen
}

/// The time `Outgoing::schedule` carries. `Ok(None)` is an ordinary send.
/// A chosen time the network will not take, or one that has passed, is an error:
/// the message stays in the field.
pub fn for_send(allowed: bool, at: Option<i64>, now: i64) -> Result<Option<i64>, &'static str> {
    match at {
        None => Ok(None),
        Some(_) if !allowed => Err("This network does not schedule messages."),
        Some(at) if at == protocol::SCHEDULE_WHEN_ONLINE => Ok(Some(at)),
        Some(at) if at <= now => Err("That time has passed."),
        Some(at) => Ok(Some(at)),
    }
}

/// In 1 hour, tomorrow at 9:00, and Monday at 9:00, in the machine's time zone.
/// Monday at 9:00 is today when it is still ahead, and next week once that hour has started.
pub fn presets(now: i64) -> Vec<Moment> {
    let mut out = Vec::with_capacity(3);
    out.push(Moment {
        label: "In 1 hour".to_owned(),
        at: now.saturating_add(3600),
    });
    if let Some(at) = at_offset(now, 1, 9, 0) {
        out.push(Moment {
            label: "Tomorrow at 9:00".to_owned(),
            at,
        });
    }
    let here = crate::gui::time::local(now);
    let days = days_until_monday(here.weekday, here.hour, here.minute);
    if let Some(at) = at_offset(now, days, 9, 0) {
        out.push(Moment {
            label: "Monday at 9:00".to_owned(),
            at,
        });
    }
    out
}

/// Up to seven coming days that still have an hour between 8:00 and 21:00.
pub fn days(now: i64) -> Vec<Day> {
    let mut out = Vec::new();
    for offset in 0..14 {
        if out.len() == 7 {
            break;
        }
        let here = crate::gui::time::local(now);
        let (year, month, day) = add_days(here.year, here.month as i32 + 1, here.day, offset);
        let day = Day { year, month, day };
        if !hours(now, day).is_empty() {
            out.push(day);
        }
    }
    out
}

/// Hours on `day` that are still ahead of `now`, from 8:00 through 21:00.
pub fn hours(now: i64, day: Day) -> Vec<Moment> {
    let mut out = Vec::new();
    for hour in 8..=21 {
        if let Some(at) = unix_at(day.year, day.month, day.day, hour, 0) {
            if at > now {
                out.push(Moment {
                    label: format!("{hour}:00"),
                    at,
                });
            }
        }
    }
    out
}

/// Today, tomorrow, or the weekday. Noon on that day names the weekday, so a zone shift
/// across midnight does not change which name is used.
pub fn day_label(now: i64, day: Day) -> String {
    let here = crate::gui::time::local(now);
    let today = (here.year, here.month as i32 + 1, here.day);
    let tomorrow = add_days(today.0, today.1, today.2, 1);
    if (day.year, day.month, day.day) == today {
        return "Today".to_owned();
    }
    if (day.year, day.month, day.day) == tomorrow {
        return "Tomorrow".to_owned();
    }
    match unix_at(day.year, day.month, day.day, 12, 0) {
        Some(at) => WEEKDAYS[crate::gui::time::local(at).weekday % 7].to_owned(),
        None => format!("{} {}", day.month, day.day),
    }
}

pub fn preset_id(index: usize) -> &'static str {
    ["schedule-hour", "schedule-tomorrow", "schedule-monday"]
        .get(index)
        .copied()
        .unwrap_or("schedule-preset")
}

pub fn day_id(index: usize) -> &'static str {
    const IDS: [&str; 7] = [
        "schedule-day-0",
        "schedule-day-1",
        "schedule-day-2",
        "schedule-day-3",
        "schedule-day-4",
        "schedule-day-5",
        "schedule-day-6",
    ];
    IDS.get(index).copied().unwrap_or("schedule-day")
}

pub fn hour_id(index: usize) -> &'static str {
    const IDS: [&str; 14] = [
        "schedule-h-0",
        "schedule-h-1",
        "schedule-h-2",
        "schedule-h-3",
        "schedule-h-4",
        "schedule-h-5",
        "schedule-h-6",
        "schedule-h-7",
        "schedule-h-8",
        "schedule-h-9",
        "schedule-h-10",
        "schedule-h-11",
        "schedule-h-12",
        "schedule-h-13",
    ];
    IDS.get(index).copied().unwrap_or("schedule-h")
}

/// The lab's schedule-send driver.
pub(crate) fn prove() -> Result<(), &'static str> {
    let now = 1_700_000_000;
    let presets = presets(now);
    let hour = presets
        .iter()
        .find(|moment| moment.label == "In 1 hour")
        .ok_or("in 1 hour is missing")?;
    if hour.at != now + 3600 {
        return Err("in 1 hour is not an hour ahead");
    }
    let tomorrow = presets
        .iter()
        .find(|moment| moment.label == "Tomorrow at 9:00")
        .ok_or("tomorrow is missing")?;
    let local = crate::gui::time::local(tomorrow.at);
    if local.hour != 9 || local.minute != 0 {
        return Err("tomorrow is not 9:00");
    }
    let today = crate::gui::time::local(now);
    let next = add_days(today.year, today.month as i32 + 1, today.day, 1);
    if (local.year, local.month as i32 + 1, local.day) != next {
        return Err("tomorrow is not the next day");
    }
    let monday = presets
        .iter()
        .find(|moment| moment.label == "Monday at 9:00")
        .ok_or("Monday is missing")?;
    let local = crate::gui::time::local(monday.at);
    if local.weekday != 1 || local.hour != 9 || local.minute != 0 || monday.at <= now {
        return Err("Monday is not a future Monday at 9:00");
    }
    if for_send(false, Some(now + 60), now) != Err("This network does not schedule messages.") {
        return Err("a network that does not schedule was given a time");
    }
    if for_send(true, Some(now), now) != Err("That time has passed.") {
        return Err("a time already past was kept");
    }
    if for_send(true, Some(now + 60), now) != Ok(Some(now + 60)) {
        return Err("a future time on a network that schedules was dropped");
    }
    if for_send(true, None, now) != Ok(None) {
        return Err("an ordinary send was given a time");
    }
    if !send_menu_for(false, true) || !send_menu_for(true, false) || send_menu_for(false, false) {
        return Err("the send menu does not follow who can schedule");
    }
    let offered = days(now);
    if offered.is_empty() || offered.len() > 7 {
        return Err("the picker does not offer a week of days");
    }
    let hours = hours(now, offered[0]);
    if hours.is_empty() || hours.iter().any(|moment| moment.at <= now) {
        return Err("the picker offers a time that has passed");
    }
    Ok(())
}

/// How many days until Monday 9:00 is still ahead. `weekday` is 0 for Sunday.
fn days_until_monday(weekday: usize, hour: i32, minute: i32) -> i32 {
    let days = (1 - weekday as i32).rem_euclid(7);
    if days == 0 && hour * 60 + minute >= 9 * 60 {
        7
    } else {
        days
    }
}

fn at_offset(now: i64, days: i32, hour: i32, minute: i32) -> Option<i64> {
    let here = crate::gui::time::local(now);
    let (year, month, day) = add_days(here.year, here.month as i32 + 1, here.day, days);
    unix_at(year, month, day, hour, minute)
}

fn add_days(mut year: i32, mut month: i32, mut day: i32, mut extra: i32) -> (i32, i32, i32) {
    if extra < 0 {
        extra = 0;
    }
    while extra > 0 {
        let room = days_in(year, month) - day;
        if extra <= room {
            day += extra;
            break;
        }
        extra -= room + 1;
        day = 1;
        month += 1;
        if month > 12 {
            month = 1;
            year += 1;
        }
    }
    (year, month, day)
}

fn days_in(year: i32, month: i32) -> i32 {
    const DAYS: [i32; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if month == 2 && leap(year) {
        29
    } else {
        DAYS[(month as usize).saturating_sub(1) % 12]
    }
}

fn leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

pub(in crate::gui) fn unix_at(year: i32, month: i32, day: i32, hour: i32, minute: i32) -> Option<i64> {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = year - 1900;
    tm.tm_mon = month - 1;
    tm.tm_mday = day;
    tm.tm_hour = hour;
    tm.tm_min = minute;
    tm.tm_isdst = -1;
    let seconds = make_local(&mut tm);
    (seconds >= 0).then_some(seconds as i64)
}

fn make_local(tm: &mut libc::tm) -> libc::time_t {
    // SAFETY: `tm` is a `libc::tm` we own. `mktime` only reads it and writes the normalized fields back.
    unsafe {
        #[cfg(unix)]
        {
            libc::mktime(tm)
        }
        #[cfg(windows)]
        {
            extern "C" {
                fn mktime(tm: *mut libc::tm) -> libc::time_t;
            }
            mktime(tm)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_send_menu_schedules_only_when_the_network_can() {
        prove().unwrap();
    }

    #[test]
    fn monday_morning_is_today_and_monday_afternoon_is_next_week() {
        assert_eq!(days_until_monday(1, 8, 59), 0);
        assert_eq!(days_until_monday(1, 9, 0), 7);
        assert_eq!(days_until_monday(2, 8, 0), 6);
        assert_eq!(days_until_monday(0, 23, 0), 1);
        assert_eq!(days_until_monday(6, 12, 0), 2);
    }

    #[test]
    fn a_day_added_across_a_month_and_a_leap_day_lands_on_the_next_date() {
        assert_eq!(add_days(2024, 2, 28, 1), (2024, 2, 29));
        assert_eq!(add_days(2024, 2, 28, 2), (2024, 3, 1));
        assert_eq!(add_days(2025, 2, 28, 1), (2025, 3, 1));
        assert_eq!(add_days(2025, 12, 31, 1), (2026, 1, 1));
        assert_eq!(add_days(2026, 1, 15, 0), (2026, 1, 15));
    }

    #[test]
    fn send_when_online_applies_only_to_visible_telegram_dms() {
        assert!(when_online_for(protocol::Network::Telegram, protocol::ConversationKind::Direct, false, false));
        assert!(!when_online_for(protocol::Network::Telegram, protocol::ConversationKind::Direct, true, false));
        assert!(!when_online_for(protocol::Network::Telegram, protocol::ConversationKind::Direct, false, true));
        assert!(!when_online_for(protocol::Network::Telegram, protocol::ConversationKind::Group, false, false));
        assert!(!when_online_for(protocol::Network::Slack, protocol::ConversationKind::Direct, false, false));
        assert_eq!(for_send(true, Some(protocol::SCHEDULE_WHEN_ONLINE), 1_700_000_000), Ok(Some(protocol::SCHEDULE_WHEN_ONLINE)));
    }
}
