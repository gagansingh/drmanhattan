//! Points in time as the files record them: RFC 3339 in UTC with milliseconds
//! (`2026-11-01T21:28:53.123Z`), held internally as a `SystemTime`.

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Timestamp(SystemTime);

impl Timestamp {
    pub(crate) fn now() -> Timestamp {
        Timestamp(SystemTime::now())
    }

    /// Seconds since the Unix epoch, for the dashboard's JavaScript.
    pub(crate) fn epoch_seconds(self) -> f64 {
        self.0.duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
    }

    /// How long ago this was (zero if it is in the future).
    pub(crate) fn elapsed(self) -> Duration {
        SystemTime::now().duration_since(self.0).unwrap_or_default()
    }

    /// The time from `self` to `later` (zero if `later` is earlier).
    pub(crate) fn until(self, later: Timestamp) -> Duration {
        later.0.duration_since(self.0).unwrap_or_default()
    }

    pub(crate) fn plus(self, d: Duration) -> Timestamp {
        Timestamp(self.0 + d)
    }

    #[cfg(test)]
    pub(crate) fn minus(self, d: Duration) -> Timestamp {
        Timestamp(self.0.checked_sub(d).unwrap_or(UNIX_EPOCH))
    }

    /// `YYYY-MM`, for the archive's month directories.
    pub(crate) fn month(self) -> String {
        DateTime::<Utc>::from(self.0).format("%Y-%m").to_string()
    }

    /// The local time of day for log lines: `14:52:34`.
    pub(crate) fn local_time(self) -> String {
        DateTime::<chrono::Local>::from(self.0).format("%H:%M:%S").to_string()
    }

    /// The local wall-clock time, e.g. `3:05 PM`.
    pub(crate) fn local_clock(self) -> String {
        DateTime::<chrono::Local>::from(self.0).format("%-I:%M %p").to_string()
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&DateTime::<Utc>::from(self.0).to_rfc3339_opts(SecondsFormat::Millis, true))
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl std::str::FromStr for Timestamp {
    type Err = chrono::ParseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Ok(Timestamp(DateTime::parse_from_rfc3339(text)?.with_timezone(&Utc).into()))
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?.parse().map_err(serde::de::Error::custom)
    }
}

/// A duration as people say it: `120ms`, `2.4s`, `45s`, `3m 05s`, `1h 02m`. Tenths are cut,
/// never rounded up, so 9.99 s reads `9.9s`, not `10.0s`.
pub(crate) fn human(d: Duration) -> String {
    let ms = d.as_millis();
    if ms < 1000 {
        return format!("{ms}ms");
    }
    if ms < 10_000 {
        return format!("{}.{}s", ms / 1000, ms % 1000 / 100);
    }
    let s = d.as_secs();
    match (s / 3600, s % 3600 / 60, s % 60) {
        (0, 0, x) => format!("{x}s"),
        (0, m, x) => format!("{m}m {x:02}s"),
        (h, m, _) => format!("{h}h {m:02}m"),
    }
}

/// `1 job`, `3 jobs`.
pub(crate) fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_round_trip_through_json() {
        let t = Timestamp(UNIX_EPOCH + Duration::from_millis(1_793_568_534_123));
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(json, "\"2026-11-01T21:28:54.123Z\"");
        assert_eq!(serde_json::from_str::<Timestamp>(&json).unwrap(), t);
        assert!(serde_json::from_str::<Timestamp>("\"yesterday\"").is_err());
        assert_eq!(t.month(), "2026-11");
    }

    #[test]
    fn durations_read_as_people_say_them() {
        let ms = |n| human(Duration::from_millis(n));
        assert_eq!([ms(0), ms(120), ms(999)], ["0ms", "120ms", "999ms"]);
        assert_eq!([ms(1000), ms(2449), ms(9999)], ["1.0s", "2.4s", "9.9s"]);
        assert_eq!([ms(10_000), ms(59_999), ms(60_000)], ["10s", "59s", "1m 00s"]);
        assert_eq!([ms(185_000), ms(3_600_000), ms(3_725_000)], ["3m 05s", "1h 00m", "1h 02m"]);
    }
}
