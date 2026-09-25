//! Per-provider request pacing and `Retry-After` interpretation.
//!
//! A [`Pacer`] spaces one provider's requests by a minimum interval and
//! holds them off after the provider answers with `Retry-After`, on the
//! tokio clock. A claim that would have to wait past the caller's deadline
//! fails at once instead of sleeping, so a paced attempt never outlives its
//! deadline. [`retry_after`] reads the header in both forms RFC 9110
//! section 10.2.3 allows: delay-seconds, and an HTTP-date in any of the
//! three formats section 5.6.7 requires recipients to accept.

use std::time::Duration;

use jiff::civil::DateTime;
use jiff::tz::TimeZone;
use jiff::{Span, Timestamp};
use tokio::sync::Mutex;
use tokio::time::Instant;

/// Longest hold a single `Retry-After` can impose. Longer values are
/// clamped so that instant arithmetic cannot overflow; any caller deadline
/// falls far inside it.
const MAX_HOLD: Duration = Duration::from_secs(365 * 24 * 60 * 60);

const SHORT_DAY_NAMES: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const LONG_DAY_NAMES: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];
const MONTH_NAMES: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

const YEAR_DIGITS: usize = 4;
const TWO_DIGITS: usize = 2;
const YEARS_PER_CENTURY: i16 = 100;
/// RFC 9110 section 5.6.7: an rfc850-date more than this many years in the
/// future names the most recent past year with the same two digits.
const RFC850_FUTURE_LIMIT_YEARS: i16 = 50;

/// Paces one provider's requests.
///
/// The only state is the earliest instant the next request may start. A
/// claim takes the slot only at the moment it returns, so dropping a claim
/// while it waits leaves the slot for the next caller.
#[derive(Debug)]
pub(crate) struct Pacer {
    min_interval: Duration,
    next_start: Mutex<Option<Instant>>,
}

/// The provider's next request slot opens after the caller's deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SlotAfterDeadline {
    /// Time from the failed claim until the slot opens.
    pub(crate) opens_in: Duration,
}

impl Pacer {
    /// A pacer that starts requests at least `min_interval` apart. Zero
    /// paces only by `Retry-After`.
    pub(crate) fn new(min_interval: Duration) -> Self {
        Self {
            min_interval,
            next_start: Mutex::new(None),
        }
    }

    /// Wait for the next request slot and take it.
    ///
    /// Cancellation-safe: the slot is taken only when this returns `Ok`,
    /// so a dropped claim consumes nothing.
    ///
    /// # Errors
    ///
    /// [`SlotAfterDeadline`], without waiting, when the slot opens after
    /// `deadline`.
    pub(crate) async fn claim(&self, deadline: Instant) -> Result<(), SlotAfterDeadline> {
        loop {
            let opens_at = {
                let mut next_start = self.next_start.lock().await;
                let now = Instant::now();
                match *next_start {
                    Some(opens_at) if opens_at > now => opens_at,
                    _ => {
                        *next_start = Some(later(now, self.min_interval));
                        return Ok(());
                    }
                }
            };
            if opens_at > deadline {
                return Err(SlotAfterDeadline {
                    opens_in: opens_at.saturating_duration_since(Instant::now()),
                });
            }
            tokio::time::sleep_until(opens_at).await;
        }
    }

    /// Hold the next request back until at least `delay` from now, as a
    /// `Retry-After` asks. A later slot already set is kept.
    pub(crate) async fn hold_for(&self, delay: Duration) {
        let until = later(Instant::now(), delay.min(MAX_HOLD));
        let mut next_start = self.next_start.lock().await;
        if next_start.is_none_or(|current| current < until) {
            *next_start = Some(until);
        }
    }
}

/// `at + delay`, saturating at the longest representable hold.
fn later(at: Instant, delay: Duration) -> Instant {
    at.checked_add(delay)
        .or_else(|| at.checked_add(MAX_HOLD))
        .unwrap_or(at)
}

/// The delay a `Retry-After` value asks for, measured from `now`.
///
/// Delay-seconds are taken as given (saturating when too large to
/// represent); an HTTP-date in the past is zero. `None` when the value is
/// neither form, which leaves the back-off to the caller.
///
/// NOTE: the day name of an HTTP-date must be a valid name for its format
/// but is not checked against the date.
pub(crate) fn retry_after(value: &str, now: Timestamp) -> Option<Duration> {
    let value = value.trim();
    if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) {
        let seconds = value.parse::<u64>().unwrap_or(u64::MAX);
        return Some(Duration::from_secs(seconds));
    }
    let at = http_date(value, now)?;
    Some(Duration::try_from(now.duration_until(at)).unwrap_or(Duration::ZERO))
}

/// An HTTP-date in IMF-fixdate, rfc850-date, or asctime-date form.
fn http_date(value: &str, now: Timestamp) -> Option<Timestamp> {
    let fields: Vec<&str> = value.split_whitespace().collect();
    let datetime = match fields.as_slice() {
        // NOTE: IMF-fixdate, `Sun, 06 Nov 1994 08:49:37 GMT`.
        [day, dd, month, year, time, "GMT"] => {
            day_name(day.strip_suffix(',')?, &SHORT_DAY_NAMES)?;
            let year = digits::<i16>(year, YEAR_DIGITS)?;
            civil(year, month_number(month)?, digits(dd, TWO_DIGITS)?, time)?
        }
        // NOTE: rfc850-date, `Sunday, 06-Nov-94 08:49:37 GMT`.
        [day, date, time, "GMT"] => {
            day_name(day.strip_suffix(',')?, &LONG_DAY_NAMES)?;
            let mut parts = date.split('-');
            let (dd, month, yy) = (parts.next()?, parts.next()?, parts.next()?);
            if parts.next().is_some() {
                return None;
            }
            let yy = digits::<i16>(yy, TWO_DIGITS)?;
            let (month, dd) = (month_number(month)?, digits(dd, TWO_DIGITS)?);
            rfc850_datetime(yy, month, dd, time, now)?
        }
        // NOTE: asctime-date, `Sun Nov  6 08:49:37 1994`.
        [day, month, dd, time, year] => {
            day_name(day, &SHORT_DAY_NAMES)?;
            let dd = digits(dd, TWO_DIGITS).or_else(|| digits(dd, 1))?;
            let year = digits::<i16>(year, YEAR_DIGITS)?;
            civil(year, month_number(month)?, dd, time)?
        }
        _ => return None,
    };
    datetime
        .to_zoned(TimeZone::UTC)
        .ok()
        .map(|zoned| zoned.timestamp())
}

/// An rfc850-date's two-digit year in the century that places it no more
/// than fifty years after `now`.
fn rfc850_datetime(yy: i16, month: i8, dd: i8, time: &str, now: Timestamp) -> Option<DateTime> {
    let today = now.to_zoned(TimeZone::UTC).datetime();
    let century = today.year() - today.year().rem_euclid(YEARS_PER_CENTURY);
    let candidate = civil(century + yy, month, dd, time)?;
    let limit = today
        .checked_add(Span::new().years(RFC850_FUTURE_LIMIT_YEARS))
        .ok()?;
    if candidate > limit {
        return civil(century - YEARS_PER_CENTURY + yy, month, dd, time);
    }
    Some(candidate)
}

fn day_name(name: &str, names: &[&str; 7]) -> Option<()> {
    names.contains(&name).then_some(())
}

fn month_number(name: &str) -> Option<i8> {
    let index = MONTH_NAMES.iter().position(|m| *m == name)?;
    i8::try_from(index + 1).ok()
}

/// Exactly `len` ASCII digits, parsed.
fn digits<T: std::str::FromStr>(text: &str, len: usize) -> Option<T> {
    (text.len() == len && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

/// A civil date and `HH:MM:SS` time of day.
fn civil(year: i16, month: i8, day: i8, time: &str) -> Option<DateTime> {
    let mut parts = time.split(':');
    let (hour, minute, second) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    DateTime::new(
        year,
        month,
        day,
        digits(hour, TWO_DIGITS)?,
        digits(minute, TWO_DIGITS)?,
        digits(second, TWO_DIGITS)?,
        0,
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    const NOW: &str = "2026-09-25T18:00:00Z";

    #[test]
    fn delay_seconds_are_taken_as_given() {
        assert_eq!(
            retry_after("120", at(NOW)),
            Some(Duration::from_secs(120)),
            "delay-seconds"
        );
        assert_eq!(
            retry_after(" 0 ", at(NOW)),
            Some(Duration::ZERO),
            "zero, surrounding whitespace ignored"
        );
        assert_eq!(
            retry_after("99999999999999999999999", at(NOW)),
            Some(Duration::from_secs(u64::MAX)),
            "an unrepresentable delay saturates rather than vanishing"
        );
    }

    #[test]
    fn all_three_http_date_forms_are_read() {
        let expected = Some(Duration::from_secs(30));
        for value in [
            "Fri, 25 Sep 2026 18:00:30 GMT",
            "Friday, 25-Sep-26 18:00:30 GMT",
            "Fri Sep 25 18:00:30 2026",
        ] {
            assert_eq!(retry_after(value, at(NOW)), expected, "{value:?}");
        }
    }

    #[test]
    fn asctime_accepts_a_space_padded_day() {
        assert_eq!(
            retry_after("Sat Oct  3 18:00:00 2026", at(NOW)),
            Some(Duration::from_secs(8 * 24 * 60 * 60)),
            "`Oct  3` is the padded single-digit day"
        );
    }

    #[test]
    fn a_date_in_the_past_is_zero() {
        assert_eq!(
            retry_after("Sun, 06 Nov 1994 08:49:37 GMT", at(NOW)),
            Some(Duration::ZERO),
            "retry now"
        );
    }

    #[test]
    fn rfc850_two_digit_years_resolve_within_fifty_years() {
        assert_eq!(
            http_date("Sunday, 06-Nov-94 08:49:37 GMT", at(NOW)),
            Some(at("1994-11-06T08:49:37Z")),
            "94 would be 2094, more than fifty years ahead, so it is 1994"
        );
        assert_eq!(
            http_date("Thursday, 01-Jan-70 00:00:00 GMT", at(NOW)),
            Some(at("2070-01-01T00:00:00Z")),
            "70 is 2070, within fifty years"
        );
    }

    #[test]
    fn values_in_neither_form_are_none() {
        for value in [
            "",
            "soon",
            "-5",
            "1.5",
            "Fri, 25 Sep 2026 18:00:30 UTC",
            "Fri, 31 Sep 2026 18:00:30 GMT",
            "Fri, 25 Sep 2026 18:00 GMT",
            "fri, 25 Sep 2026 18:00:30 GMT",
            "Friday, 25-Sep-2026 18:00:30 GMT",
            "Fri Sep 25 18:00:30 26",
        ] {
            assert_eq!(retry_after(value, at(NOW)), None, "{value:?}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn claims_are_spaced_by_the_minimum_interval() {
        let pacer = Pacer::new(Duration::from_secs(3));
        let start = Instant::now();
        let deadline = start + Duration::from_secs(60);
        pacer.claim(deadline).await.unwrap();
        assert_eq!(start.elapsed(), Duration::ZERO, "the first slot is free");
        pacer.claim(deadline).await.unwrap();
        assert_eq!(
            start.elapsed(),
            Duration::from_secs(3),
            "the second waits one interval"
        );
        pacer.claim(deadline).await.unwrap();
        assert_eq!(
            start.elapsed(),
            Duration::from_secs(6),
            "the third waits another"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_zero_interval_does_not_pace() {
        let pacer = Pacer::new(Duration::ZERO);
        let start = Instant::now();
        for _ in 0..3 {
            pacer.claim(start).await.unwrap();
        }
        assert_eq!(start.elapsed(), Duration::ZERO, "no waiting at all");
    }

    #[tokio::test(start_paused = true)]
    async fn a_dropped_waiting_claim_does_not_consume_the_slot() {
        let pacer = Pacer::new(Duration::from_secs(3));
        let start = Instant::now();
        let deadline = start + Duration::from_secs(60);
        pacer.claim(deadline).await.unwrap();

        let abandoned = tokio::time::timeout(Duration::from_secs(1), pacer.claim(deadline)).await;
        assert!(abandoned.is_err(), "the waiting claim is dropped at 1 s");

        pacer.claim(deadline).await.unwrap();
        assert_eq!(
            start.elapsed(),
            Duration::from_secs(3),
            "the next claim gets the slot at 3 s, not 6 s"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_slot_after_the_deadline_fails_without_waiting() {
        let pacer = Pacer::new(Duration::from_secs(3));
        let start = Instant::now();
        pacer.claim(start + Duration::from_secs(60)).await.unwrap();
        let err = pacer
            .claim(start + Duration::from_secs(1))
            .await
            .unwrap_err();
        assert_eq!(
            err,
            SlotAfterDeadline {
                opens_in: Duration::from_secs(3)
            },
            "the claim reports when the slot opens"
        );
        assert_eq!(start.elapsed(), Duration::ZERO, "and did not sleep");
    }

    #[tokio::test(start_paused = true)]
    async fn a_hold_delays_the_next_claim_and_never_shortens_one() {
        let pacer = Pacer::new(Duration::ZERO);
        let start = Instant::now();
        pacer.hold_for(Duration::from_secs(10)).await;
        pacer.hold_for(Duration::from_secs(2)).await;
        pacer.claim(start + Duration::from_secs(60)).await.unwrap();
        assert_eq!(
            start.elapsed(),
            Duration::from_secs(10),
            "the longer hold stands"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_claims_take_successive_slots() {
        let pacer = Pacer::new(Duration::from_secs(3));
        let start = Instant::now();
        let deadline = start + Duration::from_secs(60);
        let pacer = &pacer;
        let claim = move || async move {
            pacer.claim(deadline).await.unwrap();
            start.elapsed()
        };
        let (a, b, c) = tokio::join!(claim(), claim(), claim());
        let mut starts = [a, b, c];
        starts.sort();
        assert_eq!(
            starts,
            [
                Duration::ZERO,
                Duration::from_secs(3),
                Duration::from_secs(6)
            ],
            "three concurrent claims start one interval apart"
        );
    }
}
