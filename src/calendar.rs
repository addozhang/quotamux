//! Weekly local-time schedules shared by routing and its read-only preview.
use chrono::{DateTime, Datelike, Timelike, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

const DAY: u32 = 24 * 60 * 60;
const WEEK: u32 = 7 * DAY;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Schedule {
    pub name: String,
    pub days: Vec<u8>,
    pub start: String,
    pub end: String,
    pub order: Vec<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Selection {
    pub active_schedule: Option<String>,
    pub order: Vec<usize>,
}

#[derive(Clone, Debug)]
struct Window {
    start: u32,
    end: u32,
    schedule: usize,
}

fn wall_seconds(value: &str) -> Result<u32, String> {
    let bytes = value.as_bytes();
    if !matches!(bytes.len(), 5 | 8)
        || bytes[2] != b':'
        || (bytes.len() == 8 && bytes[5] != b':')
        || bytes
            .iter()
            .enumerate()
            .any(|(i, b)| i != 2 && !(bytes.len() == 8 && i == 5) && !b.is_ascii_digit())
    {
        return Err(format!(
            "invalid time {value:?}; expected HH:MM or HH:MM:SS"
        ));
    }
    let pair = |i| u32::from(bytes[i] - b'0') * 10 + u32::from(bytes[i + 1] - b'0');
    let hour = pair(0);
    let minute = pair(3);
    let second = if bytes.len() == 8 { pair(6) } else { 0 };
    if hour >= 24 || minute >= 60 || second >= 60 {
        return Err(format!("invalid local time {value:?}"));
    }
    Ok(hour * 3600 + minute * 60 + second)
}

fn compile(
    timezone: &str,
    layers: &[String],
    schedules: &[Schedule],
) -> Result<(Tz, Vec<Window>), String> {
    let timezone = timezone
        .parse::<Tz>()
        .map_err(|_| "timezone must be a valid IANA timezone".to_string())?;
    let layer_names: HashSet<_> = layers.iter().collect();
    if layers.is_empty()
        || layer_names.len() != layers.len()
        || layers.iter().any(|name| name.trim().is_empty())
    {
        return Err("layers must have unique nonempty names".into());
    }
    let mut names = HashSet::new();
    let mut windows = Vec::new();
    for (index, schedule) in schedules.iter().enumerate() {
        if schedule.name.trim().is_empty() || !names.insert(&schedule.name) {
            return Err("schedule names must be unique and nonempty".into());
        }
        let order: HashSet<_> = schedule.order.iter().collect();
        if schedule.order.len() != layers.len() || order != layer_names {
            return Err(format!(
                "schedule {:?} order must contain every layer exactly once",
                schedule.name
            ));
        }
        let start = wall_seconds(&schedule.start)?;
        let end = wall_seconds(&schedule.end)?;
        if start == end {
            return Err(format!(
                "schedule {:?} start and end must differ",
                schedule.name
            ));
        }
        if schedule.days.is_empty() {
            return Err(format!("schedule {:?} requires weekdays", schedule.name));
        }
        let mut days = HashSet::new();
        for &day in &schedule.days {
            if !(1..=7).contains(&day) || !days.insert(day) {
                return Err("schedule days must be unique ISO weekdays from 1 through 7".into());
            }
            let day_start = u32::from(day - 1) * DAY;
            let start_at = day_start + start;
            let end_at = day_start + end + if end < start { DAY } else { 0 };
            windows.push(Window {
                start: start_at,
                end: end_at.min(WEEK),
                schedule: index,
            });
            if end_at > WEEK {
                windows.push(Window {
                    start: 0,
                    end: end_at - WEEK,
                    schedule: index,
                });
            }
        }
    }
    windows.sort_unstable_by_key(|window| window.start);
    for pair in windows.windows(2) {
        if pair[0].end > pair[1].start {
            return Err(format!(
                "schedules {:?} and {:?} overlap",
                schedules[pair[0].schedule].name, schedules[pair[1].schedule].name
            ));
        }
    }
    Ok((timezone, windows))
}

/// Validated weekly rules and precomputed layer selections for one model.
#[derive(Clone, Debug)]
pub struct Calendar {
    timezone: Tz,
    windows: Vec<Window>,
    selections: Vec<Selection>,
    default: Selection,
}

impl Calendar {
    pub fn new(timezone: &str, layers: &[String], schedules: &[Schedule]) -> Result<Self, String> {
        let (timezone, windows) = compile(timezone, layers, schedules)?;
        let selections = schedules
            .iter()
            .map(|schedule| Selection {
                active_schedule: Some(schedule.name.clone()),
                order: schedule
                    .order
                    .iter()
                    .map(|name| {
                        layers
                            .iter()
                            .position(|layer| layer == name)
                            .expect("validated layer permutation")
                    })
                    .collect(),
            })
            .collect();
        Ok(Self {
            timezone,
            windows,
            selections,
            default: Selection {
                active_schedule: None,
                order: (0..layers.len()).collect(),
            },
        })
    }

    pub fn at(&self, at: DateTime<Utc>) -> Selection {
        let local = at.with_timezone(&self.timezone);
        let instant =
            local.weekday().num_days_from_monday() * DAY + local.num_seconds_from_midnight();
        self.windows
            .iter()
            .find(|window| window.start <= instant && instant < window.end)
            .map_or_else(
                || self.default.clone(),
                |window| self.selections[window.schedule].clone(),
            )
    }
}

pub fn validate(timezone: &str, layers: &[String], schedules: &[Schedule]) -> Result<(), String> {
    Calendar::new(timezone, layers, schedules).map(|_| ())
}

/// Compatibility entry point. Runtime callers retain a Calendar and call `at`
/// without recompiling. Invalid input defensively retains stored layer order.
pub fn evaluate(
    timezone: &str,
    layers: &[String],
    schedules: &[Schedule],
    at: DateTime<Utc>,
) -> Selection {
    Calendar::new(timezone, layers, schedules).map_or_else(
        |_| Selection {
            active_schedule: None,
            order: (0..layers.len()).collect(),
        },
        |calendar| calendar.at(at),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layers() -> Vec<String> {
        vec!["primary".into(), "backup".into()]
    }
    fn window(days: &[u8], start: &str, end: &str) -> Schedule {
        Schedule {
            name: "off-peak".into(),
            days: days.into(),
            start: start.into(),
            end: end.into(),
            order: vec!["backup".into(), "primary".into()],
        }
    }
    fn selected(zone: &str, schedules: &[Schedule], at: &str) -> Selection {
        let instant = at.parse().unwrap();
        let compiled = Calendar::new(zone, &layers(), schedules)
            .unwrap()
            .at(instant);
        assert_eq!(compiled, evaluate(zone, &layers(), schedules, instant));
        compiled
    }
    fn active(zone: &str, schedules: &[Schedule], at: &str) -> bool {
        let selection = selected(zone, schedules, at);
        if selection.active_schedule.is_some() {
            assert_eq!(selection.order, vec![1, 0]);
            true
        } else {
            assert_eq!(selection.order, vec![0, 1]);
            false
        }
    }

    #[test]
    fn second_boundaries_are_inclusive_then_exclusive() {
        let schedules = [window(&[1], "09:00:15", "12:00:30")];
        validate("UTC", &layers(), &schedules).unwrap();
        for (at, expected) in [
            ("2026-09-07T09:00:14.999Z", false),
            ("2026-09-07T09:00:15Z", true),
            ("2026-09-07T12:00:29.999Z", true),
            ("2026-09-07T12:00:30Z", false),
            ("2026-09-08T10:00:00Z", false),
        ] {
            assert_eq!(active("UTC", &schedules, at), expected, "{at}");
        }
    }

    #[test]
    fn overnight_belongs_to_start_weekday_and_wraps_week() {
        for (day, before, start, inside, end) in [
            (
                5,
                "2026-09-11T19:59:59Z",
                "2026-09-11T20:00:00Z",
                "2026-09-12T01:59:59Z",
                "2026-09-12T02:00:00Z",
            ),
            (
                7,
                "2026-09-13T19:59:59Z",
                "2026-09-13T20:00:00Z",
                "2026-09-14T01:59:59Z",
                "2026-09-14T02:00:00Z",
            ),
        ] {
            let schedules = [window(&[day], "20:00", "02:00")];
            validate("UTC", &layers(), &schedules).unwrap();
            assert!(!active("UTC", &schedules, before));
            assert!(active("UTC", &schedules, start));
            assert!(active("UTC", &schedules, inside));
            assert!(!active("UTC", &schedules, end));
        }
        let midnight = [window(&[7], "23:00", "00:00")];
        validate("UTC", &layers(), &midnight).unwrap();
        assert!(!active("UTC", &midnight, "2026-09-14T00:00:00Z"));
    }

    #[test]
    fn neighboring_windows_are_valid_but_overlaps_are_not() {
        let first = window(&[1], "09:00", "12:00");
        let mut next = window(&[1], "12:00", "14:00");
        next.name = "afternoon".into();
        validate("UTC", &layers(), &[first.clone(), next.clone()]).unwrap();
        assert_eq!(
            selected(
                "UTC",
                &[first.clone(), next.clone()],
                "2026-09-07T12:00:00Z"
            )
            .active_schedule
            .as_deref(),
            Some("afternoon")
        );
        next.start = "11:59:59".into();
        assert!(validate("UTC", &layers(), &[first, next]).is_err());
        let sunday = window(&[7], "23:00", "02:00");
        let mut monday = window(&[1], "01:59:59", "03:00");
        monday.name = "monday".into();
        assert!(validate("UTC", &layers(), &[sunday.clone(), monday.clone()]).is_err());
        monday.start = "02:00".into();
        validate("UTC", &layers(), &[sunday, monday]).unwrap();
    }

    #[test]
    fn timezone_and_dst_follow_each_real_instant() {
        let local = [window(&[1], "09:00", "10:00")];
        assert!(active("Asia/Shanghai", &local, "2026-09-07T01:00:00Z"));
        assert!(!active("Asia/Shanghai", &local, "2026-09-07T02:00:00Z"));
        let repeated = [window(&[7], "01:15", "01:45")];
        validate("America/New_York", &layers(), &repeated).unwrap();
        // Both occurrences of 01:30 on the autumn transition match.
        assert!(active(
            "America/New_York",
            &repeated,
            "2026-11-01T05:30:00Z"
        ));
        assert!(active(
            "America/New_York",
            &repeated,
            "2026-11-01T06:30:00Z"
        ));
        assert!(!active(
            "America/New_York",
            &repeated,
            "2026-11-01T06:45:00Z"
        ));
        let skipped = [window(&[7], "02:00", "03:00")];
        validate("America/New_York", &layers(), &skipped).unwrap();
        // 01:59:59 advances to 03:00:00, never entering this wall-time window.
        assert!(!active(
            "America/New_York",
            &skipped,
            "2026-03-08T06:59:59Z"
        ));
        assert!(!active(
            "America/New_York",
            &skipped,
            "2026-03-08T07:00:00Z"
        ));
        let crossing = [window(&[7], "01:30", "03:30")];
        assert!(active(
            "America/New_York",
            &crossing,
            "2026-03-08T07:00:00Z"
        ));
        assert!(!active(
            "America/New_York",
            &crossing,
            "2026-03-08T07:30:00Z"
        ));
    }

    #[test]
    fn invalid_times_days_names_and_permutations_are_rejected() {
        for time in [
            "9:00",
            "09:0",
            "09:00:0",
            "24:00",
            "09:60",
            "09:00:60",
            "09:00 ",
            " 09:00",
            "09-00",
            "٠٩:٠٠",
            "09:00:00.1",
        ] {
            assert!(
                validate("UTC", &layers(), &[window(&[1], time, "12:00")]).is_err(),
                "{time}"
            );
        }
        assert!(validate("UTC", &layers(), &[window(&[1], "09:00", "09:00:00")]).is_err());
        for days in [vec![], vec![0], vec![8], vec![1, 1]] {
            assert!(validate("UTC", &layers(), &[window(&days, "09:00", "12:00")]).is_err());
        }
        let good = window(&[1], "09:00", "12:00");
        for order in [
            vec!["primary"],
            vec!["primary", "primary"],
            vec!["primary", "missing"],
            vec!["primary", "backup", "extra"],
        ] {
            let mut bad = good.clone();
            bad.order = order.into_iter().map(str::to_owned).collect();
            assert!(validate("UTC", &layers(), &[bad]).is_err());
        }
        let mut blank = good.clone();
        blank.name = "  ".into();
        assert!(validate("UTC", &layers(), &[blank]).is_err());
        let mut duplicate = good.clone();
        duplicate.days = vec![2];
        assert!(validate("UTC", &layers(), &[good, duplicate]).is_err());
        assert!(validate("Not/AZone", &layers(), &[]).is_err());
        assert!(validate("UTC", &["same".into(), "same".into()], &[]).is_err());
        assert!(validate("UTC", &["".into()], &[]).is_err());
    }

    #[test]
    fn absent_schedules_restore_stored_order_and_schema_is_strict() {
        validate("UTC", &layers(), &[]).unwrap();
        assert_eq!(
            selected("UTC", &[], "2026-09-07T09:00:00Z"),
            Selection {
                active_schedule: None,
                order: vec![0, 1]
            }
        );
        let schedule = window(&[1, 3, 5], "09:00", "12:00:00");
        let mut value = serde_json::to_value(&schedule).unwrap();
        assert_eq!(
            serde_json::from_value::<Schedule>(value.clone()).unwrap(),
            schedule
        );
        value["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<Schedule>(value).is_err());
    }
}
