//! Calendar thresholds for round-up-spec 2. The clock and zone are run inputs.
use serde::{Deserialize, Serialize};

const HOUR: i64 = 3_600_000;
const DAY: i64 = 24 * HOUR;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeUnit {
    Hours,
    Days,
    Weeks,
    Months,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Age {
    pub n: u32,
    pub unit: TimeUnit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clock {
    pub now_ms: i64,
}
impl Clock {
    pub fn now() -> Self {
        let now_ms = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => i64::try_from(d.as_millis()).unwrap_or(i64::MAX),
            Err(e) => -i64::try_from(e.duration().as_millis()).unwrap_or(i64::MAX),
        };
        Self { now_ms }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LocalDateTime {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub millisecond: u32,
}

pub trait TimeZone {
    fn to_local(&self, utc_ms: i64) -> Option<LocalDateTime>;
    /// Round-up-spec 2: choose the earlier overlap instant; shift gaps forward
    /// by their duration, preserving minutes/seconds within the missing interval.
    fn resolve_local(&self, local: LocalDateTime) -> Option<i64>;
}

pub fn threshold(clock: Clock, age: Age, zone: &dyn TimeZone) -> Option<i64> {
    if age.unit == TimeUnit::Hours {
        return clock
            .now_ms
            .checked_sub(i64::from(age.n).checked_mul(HOUR)?);
    }
    let mut local = zone.to_local(clock.now_ms)?;
    match age.unit {
        TimeUnit::Days | TimeUnit::Weeks => {
            let days =
                i64::from(age.n).checked_mul(if age.unit == TimeUnit::Weeks { 7 } else { 1 })?;
            let epoch_day =
                days_from_civil(local.year, local.month, local.day)?.checked_sub(days)?;
            (local.year, local.month, local.day) = civil_from_days(epoch_day)?;
        }
        TimeUnit::Months => {
            let month_index = i64::from(local.year)
                .checked_mul(12)?
                .checked_add(i64::from(local.month) - 1)?
                .checked_sub(i64::from(age.n))?;
            local.year = i32::try_from(month_index.div_euclid(12)).ok()?;
            local.month = u32::try_from(month_index.rem_euclid(12) + 1).ok()?;
            local.day = local.day.min(month_days(local.year, local.month)?);
        }
        TimeUnit::Hours => unreachable!(),
    }
    zone.resolve_local(local)
}

fn month_days(year: i32, month: u32) -> Option<u32> {
    Some(match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year.rem_euclid(4) == 0
            && (year.rem_euclid(100) != 0 || year.rem_euclid(400) == 0) =>
        {
            29
        }
        2 => 28,
        _ => return None,
    })
}

// Gregorian era decomposition, with a Unix day origin. Every conversion checks
// its range; an unsupported calendar value becomes Could-not-evaluate upstream.
fn days_from_civil(year: i32, month: u32, day: u32) -> Option<i64> {
    if day == 0 || day > month_days(year, month)? {
        return None;
    }
    let year = i64::from(year) - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_in_era = year - era * 400;
    let march_month = i64::from(month) + if month > 2 { -3 } else { 9 };
    let day_in_year = (153 * march_month + 2) / 5 + i64::from(day) - 1;
    Some(
        era * 146097 + year_in_era * 365 + year_in_era / 4 - year_in_era / 100 + day_in_year
            - 719468,
    )
}

fn civil_from_days(days: i64) -> Option<(i32, u32, u32)> {
    let days = days.checked_add(719468)?;
    let era = days.div_euclid(146097);
    let day_in_era = days - era * 146097;
    let year_in_era =
        (day_in_era - day_in_era / 1460 + day_in_era / 36524 - day_in_era / 146096) / 365;
    let day_in_year = day_in_era - (365 * year_in_era + year_in_era / 4 - year_in_era / 100);
    let march_month = (5 * day_in_year + 2) / 153;
    let day = day_in_year - (153 * march_month + 2) / 5 + 1;
    let month = march_month + if march_month < 10 { 3 } else { -9 };
    let year = era
        .checked_mul(400)?
        .checked_add(year_in_era)?
        .checked_add(i64::from(month <= 2))?;
    Some((i32::try_from(year).ok()?, month as u32, day as u32))
}

/// `YYYYMMDD-HHMMSS` in UTC, for file names of configuration copies (round-up-spec 6).
pub fn utc_stamp(utc_ms: i64) -> String {
    let (year, month, day) = civil_from_days(utc_ms.div_euclid(DAY)).unwrap_or((1970, 1, 1));
    let seconds = utc_ms.rem_euclid(DAY) / 1000;
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

fn naive_ms(local: LocalDateTime) -> Option<i64> {
    if local.hour > 23 || local.minute > 59 || local.second > 59 || local.millisecond > 999 {
        return None;
    }
    days_from_civil(local.year, local.month, local.day)?
        .checked_mul(DAY)?
        .checked_add(
            i64::from(local.hour) * HOUR
                + i64::from(local.minute) * 60_000
                + i64::from(local.second) * 1000
                + i64::from(local.millisecond),
        )
}

fn local_from_naive(ms: i64) -> Option<LocalDateTime> {
    let (year, month, day) = civil_from_days(ms.div_euclid(DAY))?;
    let time = ms.rem_euclid(DAY);
    Some(LocalDateTime {
        year,
        month,
        day,
        hour: (time / HOUR) as u32,
        minute: (time / 60_000 % 60) as u32,
        second: (time / 1000 % 60) as u32,
        millisecond: (time % 1000) as u32,
    })
}

/// Resolve candidates by round-trip: all matching instants are considered, so
/// overlap handling never depends on the operating system's preferred side.
fn resolve_candidates(
    zone: &dyn TimeZone,
    local: LocalDateTime,
    offsets_ms: &[i64],
) -> Option<i64> {
    let naive = naive_ms(local)?;
    let mut exact = None;
    let mut forward: Option<(i64, i64)> = None;
    for offset in offsets_ms {
        let Some(candidate) = naive.checked_sub(*offset) else {
            continue;
        };
        let Some(actual) = zone.to_local(candidate).and_then(naive_ms) else {
            continue;
        };
        if actual == naive {
            exact = Some(exact.map_or(candidate, |old: i64| old.min(candidate)));
        } else if actual > naive {
            let pair = (actual - naive, candidate);
            if forward.is_none_or(|old| pair < old) {
                forward = Some(pair);
            }
        }
    }
    exact.or_else(|| forward.map(|(_, instant)| instant))
}

/// Captures the Windows zone once; no registry or clock changes are made.
pub struct LocalTimeZone {
    data: DynamicTimeZoneInformation,
}
impl LocalTimeZone {
    pub fn capture() -> Option<Self> {
        let mut data: DynamicTimeZoneInformation = unsafe { std::mem::zeroed() };
        (unsafe { GetDynamicTimeZoneInformation(&mut data) } != u32::MAX).then_some(Self { data })
    }
}
impl TimeZone for LocalTimeZone {
    fn to_local(&self, utc_ms: i64) -> Option<LocalDateTime> {
        let input = SystemTime::from_local(local_from_naive(utc_ms)?)?;
        let mut output = SystemTime::default();
        (unsafe { SystemTimeToTzSpecificLocalTimeEx(&self.data, &input, &mut output) } != 0)
            .then(|| output.as_local())
    }
    fn resolve_local(&self, local: LocalDateTime) -> Option<i64> {
        let mut offsets =
            vec![-(i64::from(self.data.bias) + i64::from(self.data.standard_bias)) * 60_000];
        if self.data.daylight_date.month != 0 {
            offsets
                .push(-(i64::from(self.data.bias) + i64::from(self.data.daylight_bias)) * 60_000);
        }
        // Include neighbouring years for transitions that change base offset.
        for year in [
            local.year.checked_sub(1)?,
            local.year,
            local.year.checked_add(1)?,
        ] {
            let Ok(year) = u16::try_from(year) else {
                continue;
            };
            let mut info: TimeZoneInformation = unsafe { std::mem::zeroed() };
            if unsafe { GetTimeZoneInformationForYear(year, &self.data, &mut info) } == 0 {
                continue;
            }
            offsets.push(-(i64::from(info.bias) + i64::from(info.standard_bias)) * 60_000);
            if info.daylight_date.month != 0 {
                offsets.push(-(i64::from(info.bias) + i64::from(info.daylight_bias)) * 60_000);
            }
        }
        offsets.sort_unstable();
        offsets.dedup();
        resolve_candidates(self, local, &offsets)
    }
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct SystemTime {
    year: u16,
    month: u16,
    weekday: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    millisecond: u16,
}
impl SystemTime {
    fn from_local(value: LocalDateTime) -> Option<Self> {
        naive_ms(value)?;
        Some(Self {
            year: u16::try_from(value.year).ok()?,
            month: value.month as u16,
            weekday: 0,
            day: value.day as u16,
            hour: value.hour as u16,
            minute: value.minute as u16,
            second: value.second as u16,
            millisecond: value.millisecond as u16,
        })
    }
    fn as_local(self) -> LocalDateTime {
        LocalDateTime {
            year: self.year.into(),
            month: self.month.into(),
            day: self.day.into(),
            hour: self.hour.into(),
            minute: self.minute.into(),
            second: self.second.into(),
            millisecond: self.millisecond.into(),
        }
    }
}
#[repr(C)]
struct TimeZoneInformation {
    bias: i32,
    standard_name: [u16; 32],
    standard_date: SystemTime,
    standard_bias: i32,
    daylight_name: [u16; 32],
    daylight_date: SystemTime,
    daylight_bias: i32,
}
#[repr(C)]
struct DynamicTimeZoneInformation {
    bias: i32,
    standard_name: [u16; 32],
    standard_date: SystemTime,
    standard_bias: i32,
    daylight_name: [u16; 32],
    daylight_date: SystemTime,
    daylight_bias: i32,
    key_name: [u16; 128],
    dynamic_daylight_time_disabled: u8,
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetDynamicTimeZoneInformation(info: *mut DynamicTimeZoneInformation) -> u32;
    fn GetTimeZoneInformationForYear(
        year: u16,
        dynamic: *const DynamicTimeZoneInformation,
        info: *mut TimeZoneInformation,
    ) -> i32;
    fn SystemTimeToTzSpecificLocalTimeEx(
        zone: *const DynamicTimeZoneInformation,
        universal: *const SystemTime,
        local: *mut SystemTime,
    ) -> i32;
}

/// Fake zones for tests (round-up-spec 7): a fixed offset and a Europe-like zone with daylight
/// saving between 2024-03-31 01:00 UTC and 2024-10-27 01:00 UTC.
#[cfg(test)]
pub(crate) mod fake {
    use super::*;

    pub const HOUR_MS: i64 = HOUR;
    pub const DAY_MS: i64 = DAY;

    /// A zone with a constant offset from UTC in milliseconds.
    pub struct Fixed(pub i64);
    impl TimeZone for Fixed {
        fn to_local(&self, instant: i64) -> Option<LocalDateTime> {
            local_from_naive(instant.checked_add(self.0)?)
        }
        fn resolve_local(&self, local: LocalDateTime) -> Option<i64> {
            naive_ms(local)?.checked_sub(self.0)
        }
    }

    pub struct Dst;
    impl TimeZone for Dst {
        fn to_local(&self, instant: i64) -> Option<LocalDateTime> {
            let start = naive_ms(date(2024, 3, 31, 1, 0))?;
            let end = naive_ms(date(2024, 10, 27, 1, 0))?;
            local_from_naive(instant.checked_add(if instant >= start && instant < end {
                2 * HOUR
            } else {
                HOUR
            })?)
        }
        fn resolve_local(&self, local: LocalDateTime) -> Option<i64> {
            resolve_candidates(self, local, &[HOUR, 2 * HOUR])
        }
    }

    pub fn date(y: i32, m: u32, d: u32, h: u32, min: u32) -> LocalDateTime {
        LocalDateTime {
            year: y,
            month: m,
            day: d,
            hour: h,
            minute: min,
            second: 0,
            millisecond: 0,
        }
    }

    /// The instant at which `zone` shows `local`.
    pub fn clock(local: LocalDateTime, zone: &dyn TimeZone) -> Clock {
        Clock {
            now_ms: zone.resolve_local(local).unwrap(),
        }
    }

    /// Milliseconds since the Unix epoch of a UTC wall-clock time.
    pub fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> i64 {
        naive_ms(date(y, m, d, h, min)).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::fake::*;
    use super::*;
    fn age(n: u32, unit: TimeUnit) -> Age {
        Age { n, unit }
    }
    #[test]
    fn utc_calendar_month_clamp_and_leap_year() {
        let zone = Fixed(0);
        assert_eq!(
            threshold(
                clock(date(2024, 3, 31, 12, 0), &zone),
                age(1, TimeUnit::Months),
                &zone
            ),
            zone.resolve_local(date(2024, 2, 29, 12, 0))
        );
        assert_eq!(
            threshold(
                clock(date(2023, 3, 31, 12, 0), &zone),
                age(1, TimeUnit::Months),
                &zone
            ),
            zone.resolve_local(date(2023, 2, 28, 12, 0))
        );
        assert_eq!(
            threshold(
                clock(date(2024, 1, 31, 12, 0), &zone),
                age(13, TimeUnit::Months),
                &zone
            ),
            zone.resolve_local(date(2022, 12, 31, 12, 0))
        );
    }
    #[test]
    fn positive_offset_calendar_weeks_and_milliseconds() {
        let zone = Fixed(5 * HOUR + 30 * 60_000);
        let mut now = date(2024, 1, 3, 0, 15);
        now.millisecond = 42;
        let mut expected = date(2023, 12, 27, 0, 15);
        expected.millisecond = 42;
        assert_eq!(
            threshold(clock(now, &zone), age(1, TimeUnit::Weeks), &zone),
            zone.resolve_local(expected)
        );
        assert_eq!(
            local_from_naive(-1).unwrap(),
            LocalDateTime {
                year: 1969,
                month: 12,
                day: 31,
                hour: 23,
                minute: 59,
                second: 59,
                millisecond: 999
            }
        );
    }
    #[test]
    fn spring_gap_shifts_forward() {
        let zone = Dst;
        let actual = threshold(
            clock(date(2024, 4, 1, 2, 30), &zone),
            age(1, TimeUnit::Days),
            &zone,
        )
        .unwrap();
        assert_eq!(zone.to_local(actual), Some(date(2024, 3, 31, 3, 30)));
        assert_eq!(actual, naive_ms(date(2024, 3, 31, 1, 30)).unwrap());
    }
    #[test]
    fn autumn_overlap_chooses_earlier_instant() {
        let zone = Dst;
        let actual = threshold(
            clock(date(2024, 10, 28, 2, 30), &zone),
            age(1, TimeUnit::Days),
            &zone,
        )
        .unwrap();
        assert_eq!(actual, naive_ms(date(2024, 10, 27, 0, 30)).unwrap());
    }
    #[test]
    fn hours_are_exact_and_days_follow_dst() {
        let zone = Dst;
        let now = clock(date(2024, 3, 31, 12, 0), &zone);
        let hours = threshold(now, age(24, TimeUnit::Hours), &zone).unwrap();
        let days = threshold(now, age(1, TimeUnit::Days), &zone).unwrap();
        assert_eq!(now.now_ms - hours, DAY);
        assert_eq!(now.now_ms - days, 23 * HOUR);
        assert_eq!(zone.to_local(hours), Some(date(2024, 3, 30, 11, 0)));
        assert_eq!(zone.to_local(days), Some(date(2024, 3, 30, 12, 0)));
    }
    #[test]
    fn roundtrip_calendar_and_typed_storage() {
        for year in [1601, 1900, 1969, 1970, 2000, 2024, 9999] {
            for month in 1..=12 {
                let day = month_days(year, month).unwrap();
                assert_eq!(
                    civil_from_days(days_from_civil(year, month, day).unwrap()),
                    Some((year, month, day))
                );
            }
        }
        let value = age(7, TimeUnit::Days);
        let json = serde_json::to_string(&value).unwrap();
        assert_eq!(json, r#"{"n":7,"unit":"days"}"#);
        assert_eq!(serde_json::from_str::<Age>(&json).unwrap(), value);
    }
    #[test]
    fn utc_stamp_formats_file_name_time() {
        assert_eq!(utc_stamp(0), "19700101-000000");
        assert_eq!(utc_stamp(1_700_000_000_000), "20231114-221320");
        assert_eq!(utc_stamp(-1), "19691231-235959");
    }

    #[test]
    fn windows_ffi_layout_matches_sdk() {
        assert_eq!(std::mem::size_of::<SystemTime>(), 16);
        assert_eq!(std::mem::size_of::<TimeZoneInformation>(), 172);
        assert_eq!(std::mem::size_of::<DynamicTimeZoneInformation>(), 432);
    }
}
