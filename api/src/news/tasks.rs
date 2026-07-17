use chrono::{DateTime, FixedOffset, NaiveDate, TimeZone, Utc};

pub fn parse_hhmm(value: &str) -> Option<u32> {
    let (hour, minute) = value.split_once(':')?;
    if hour.len() != 2 || minute.len() != 2 {
        return None;
    }
    let hour = hour.parse::<u32>().ok()?;
    let minute = minute.parse::<u32>().ok()?;
    (hour < 24 && minute < 60).then_some(hour * 60 + minute)
}

pub fn scheduled_utc(date: &str, time: &str, tz_offset_hours: i32) -> Option<DateTime<Utc>> {
    let date = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    let minutes = parse_hhmm(time)?;
    let local = date.and_hms_opt(minutes / 60, minutes % 60, 0)?;
    let offset = FixedOffset::east_opt(tz_offset_hours * 3600)?;
    offset
        .from_local_datetime(&local)
        .single()
        .map(|value| value.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hhmm_is_strict() {
        assert_eq!(parse_hhmm("08:30"), Some(510));
        assert_eq!(parse_hhmm("8:30"), None);
        assert_eq!(parse_hhmm("24:00"), None);
        assert_eq!(parse_hhmm("08:60"), None);
    }

    #[test]
    fn local_time_converts_to_utc() {
        assert_eq!(
            scheduled_utc("2026-07-17", "08:00", 8)
                .unwrap()
                .format("%Y-%m-%d %H:%M")
                .to_string(),
            "2026-07-17 00:00"
        );
        assert_eq!(
            scheduled_utc("2026-07-17", "08:00", 8).unwrap().timestamp() % 60,
            0
        );
    }
}
