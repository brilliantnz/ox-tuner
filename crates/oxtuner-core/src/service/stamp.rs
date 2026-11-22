//! The save-time stamp the reference writer puts in its file headers.
//!
//! The shape is Java's `Date.toString()`: `EEE MMM dd HH:mm:ss zzz yyyy`, with
//! English names, a two-digit day, 24-hour time and a four-digit year. UTC is
//! used so the text is deterministic and needs no time-zone database.

/// The current time formatted for a `.msq` `writeDate` or properties header.
pub(super) fn now() -> String {
    format_stamp(::time::OffsetDateTime::now_utc())
}

/// Formats an offset date in the reference writer's shape.
fn format_stamp(now: ::time::OffsetDateTime) -> String {
    // `time` spells the names in English; the reference keeps the first three
    // characters of each.
    let weekday = now.weekday().to_string();
    let month = now.month().to_string();

    format!(
        "{:.3} {:.3} {:02} {:02}:{:02}:{:02} UTC {}",
        weekday,
        month,
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
        now.year()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stamp_matches_the_java_date_shape() {
        let at = ::time::Date::from_calendar_date(2026, ::time::Month::November, 22)
            .expect("a valid date")
            .with_hms(6, 46, 58)
            .expect("a valid time")
            .assume_utc();

        assert_eq!(format_stamp(at), "Sun Nov 22 06:46:58 UTC 2026");
    }

    #[test]
    fn a_single_digit_day_is_zero_padded() {
        // A real TunerStudio file in the samples writes `Mon Mar 03 …`, so the
        // reference pads the day instead of Java's older space-padded form.
        let at = ::time::Date::from_calendar_date(2026, ::time::Month::November, 5)
            .expect("a valid date")
            .with_hms(6, 46, 58)
            .expect("a valid time")
            .assume_utc();

        assert_eq!(format_stamp(at), "Thu Nov 05 06:46:58 UTC 2026");
    }
}
