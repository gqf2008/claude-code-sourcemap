//! Minimal cron expression parsing and next-run calculation.
//!
//! Supports the standard 5-field cron subset:
//!   minute hour day-of-month month day-of-week
//!
//! Field syntax: wildcard, N, step (*/N), range (N-M), list (N,M,...).
//! No L, W, ?, or name aliases. All times are interpreted in the process's
//! local timezone.

use chrono::{Datelike, Local, NaiveDate, NaiveDateTime, TimeZone, Timelike};

/// Expanded cron fields — each is a sorted array of matching values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronFields {
    pub minute: Vec<u8>,
    pub hour: Vec<u8>,
    pub day_of_month: Vec<u8>,
    pub month: Vec<u8>,
    pub day_of_week: Vec<u8>,
}

struct FieldRange {
    min: u8,
    max: u8,
}

const FIELD_RANGES: [FieldRange; 5] = [
    FieldRange { min: 0, max: 59 },  // minute
    FieldRange { min: 0, max: 23 },  // hour
    FieldRange { min: 1, max: 31 },  // day_of_month
    FieldRange { min: 1, max: 12 },  // month
    FieldRange { min: 0, max: 6 },   // day_of_week (0=Sunday; 7 accepted as alias)
];

/// Parse a single cron field into a sorted array of matching values.
fn expand_field(field: &str, range: &FieldRange) -> Option<Vec<u8>> {
    let FieldRange { min, max } = *range;
    let is_dow = min == 0 && max == 6;
    let mut out = std::collections::BTreeSet::new();

    for part in field.split(',') {
        if part.starts_with('*') {
            // Wildcard or */N step
            let step = if let Some(rest) = part.strip_prefix("*/") {
                rest.parse::<u8>().ok().filter(|&s| s >= 1)?
            } else if part == "*" {
                1
            } else {
                return None;
            };
            let mut i = min;
            while i <= max {
                out.insert(i);
                i = i.checked_add(step)?;
                if i > max && step > 1 {
                    break;
                }
            }
        } else if part.contains('-') {
            // Range: N-M or N-M/S
            let (range_part, step_part) = if let Some((r, s)) = part.split_once('/') {
                (r, Some(s))
            } else {
                (part, None)
            };
            let (lo_s, hi_s) = range_part.split_once('-')?;
            let lo = lo_s.parse::<u8>().ok()?;
            let hi = hi_s.parse::<u8>().ok()?;
            let step = match step_part {
                Some(s) => s.parse::<u8>().ok().filter(|&v| v >= 1)?,
                None => 1,
            };
            let eff_max = if is_dow { 7 } else { max };
            if lo > hi || lo < min || hi > eff_max {
                return None;
            }
            let mut i = lo;
            while i <= hi {
                let val = if is_dow && i == 7 { 0 } else { i };
                out.insert(val);
                i = i.checked_add(step)?;
                if i > hi && step > 1 {
                    break;
                }
            }
        } else {
            // Plain number
            let mut n = part.parse::<u8>().ok()?;
            if is_dow && n == 7 {
                n = 0;
            }
            if (n < min || n > max) && !(is_dow && part == "7") {
                return None;
            }
            out.insert(n);
        }
    }

    if out.is_empty() {
        return None;
    }
    Some(out.into_iter().collect())
}

/// Parse a 5-field cron expression into expanded number arrays.
/// Returns None if invalid or unsupported syntax.
pub fn parse_cron_expression(expr: &str) -> Option<CronFields> {
    let parts: Vec<&str> = expr.split_whitespace().collect();
    if parts.len() != 5 {
        return None;
    }

    let minute = expand_field(parts[0], &FIELD_RANGES[0])?;
    let hour = expand_field(parts[1], &FIELD_RANGES[1])?;
    let day_of_month = expand_field(parts[2], &FIELD_RANGES[2])?;
    let month = expand_field(parts[3], &FIELD_RANGES[3])?;
    let day_of_week = expand_field(parts[4], &FIELD_RANGES[4])?;

    Some(CronFields {
        minute,
        hour,
        day_of_month,
        month,
        day_of_week,
    })
}

/// Compute the next local DateTime strictly after `from` that matches the cron
/// fields. Walks forward minute-by-minute. Bounded at 366 days.
///
/// Standard cron semantics: when both dayOfMonth and dayOfWeek are constrained
/// (neither is the full range), a date matches if EITHER matches.
pub fn compute_next_cron_run(fields: &CronFields, from: NaiveDateTime) -> Option<NaiveDateTime> {
    use std::collections::HashSet;

    let minute_set: HashSet<u8> = fields.minute.iter().copied().collect();
    let hour_set: HashSet<u8> = fields.hour.iter().copied().collect();
    let dom_set: HashSet<u8> = fields.day_of_month.iter().copied().collect();
    let month_set: HashSet<u8> = fields.month.iter().copied().collect();
    let dow_set: HashSet<u8> = fields.day_of_week.iter().copied().collect();

    let dom_wild = fields.day_of_month.len() == 31;
    let dow_wild = fields.day_of_week.len() == 7;

    // Round up to the next whole minute (strictly after `from`)
    let mut t = from
        .with_second(0)?
        .with_nanosecond(0)?;
    t += chrono::Duration::minutes(1);

    let max_iter = 366 * 24 * 60;
    for _ in 0..max_iter {
        let month = t.month() as u8;
        if !month_set.contains(&month) {
            // Jump to start of next month
            let (y, m) = if month == 12 {
                (t.year() + 1, 1)
            } else {
                (t.year(), month as u32 + 1)
            };
            t = NaiveDate::from_ymd_opt(y, m, 1)?
                .and_hms_opt(0, 0, 0)?;
            continue;
        }

        let dom = t.day() as u8;
        let dow = t.weekday().num_days_from_sunday() as u8;
        let day_matches = if dom_wild && dow_wild {
            true
        } else if dom_wild {
            dow_set.contains(&dow)
        } else if dow_wild {
            dom_set.contains(&dom)
        } else {
            dom_set.contains(&dom) || dow_set.contains(&dow)
        };

        if !day_matches {
            // Jump to start of next day
            t = (t.date() + chrono::Duration::days(1)).and_hms_opt(0, 0, 0)?;
            continue;
        }

        if !hour_set.contains(&(t.hour() as u8)) {
            t = t.with_minute(0)?.with_second(0)?;
            t += chrono::Duration::hours(1);
            continue;
        }

        if !minute_set.contains(&(t.minute() as u8)) {
            t += chrono::Duration::minutes(1);
            continue;
        }

        return Some(t);
    }

    None
}

/// Compute next cron run from the current local time, returning epoch millis.
pub fn next_cron_run_ms(cron: &str, from_ms: i64) -> Option<i64> {
    let fields = parse_cron_expression(cron)?;
    let from_secs = from_ms.div_euclid(1000);
    let from_nanos = (from_ms.rem_euclid(1000) * 1_000_000) as u32;
    let from_dt = chrono::DateTime::from_timestamp(from_secs, from_nanos)?;
    let local_dt = from_dt.with_timezone(&Local).naive_local();
    let next = compute_next_cron_run(&fields, local_dt)?;
    // Convert back to epoch ms via local timezone
    let local_next = Local.from_local_datetime(&next).earliest()?;
    Some(local_next.timestamp_millis())
}

// --- Human-readable descriptions ---

const DAY_NAMES: [&str; 7] = [
    "Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday",
];

fn format_local_time(minute: u32, hour: u32) -> String {
    let h12 = if hour == 0 {
        12
    } else if hour > 12 {
        hour - 12
    } else {
        hour
    };
    let ampm = if hour < 12 { "AM" } else { "PM" };
    format!("{}:{:02} {}", h12, minute, ampm)
}

/// Convert a cron expression to a human-readable description.
pub fn cron_to_human(cron: &str) -> String {
    let parts: Vec<&str> = cron.split_whitespace().collect();
    if parts.len() != 5 {
        return cron.to_string();
    }

    let (minute, hour, day_of_month, month, day_of_week) =
        (parts[0], parts[1], parts[2], parts[3], parts[4]);

    // Every N minutes: */N * * * *
    if let Some(rest) = minute.strip_prefix("*/") {
        if let Ok(n) = rest.parse::<u32>() {
            if hour == "*" && day_of_month == "*" && month == "*" && day_of_week == "*" {
                return if n == 1 {
                    "Every minute".to_string()
                } else {
                    format!("Every {} minutes", n)
                };
            }
        }
    }

    // Every hour: N * * * *
    if minute.chars().all(|c| c.is_ascii_digit())
        && hour == "*"
        && day_of_month == "*"
        && month == "*"
        && day_of_week == "*"
    {
        let m: u32 = minute.parse().unwrap_or(0);
        return if m == 0 {
            "Every hour".to_string()
        } else {
            format!("Every hour at :{:02}", m)
        };
    }

    // Every N hours: M */N * * *
    if let Some(rest) = hour.strip_prefix("*/") {
        if let Ok(n) = rest.parse::<u32>() {
            if minute.chars().all(|c| c.is_ascii_digit())
                && day_of_month == "*"
                && month == "*"
                && day_of_week == "*"
            {
                let m: u32 = minute.parse().unwrap_or(0);
                let suffix = if m == 0 {
                    String::new()
                } else {
                    format!(" at :{:02}", m)
                };
                return if n == 1 {
                    format!("Every hour{}", suffix)
                } else {
                    format!("Every {} hours{}", n, suffix)
                };
            }
        }
    }

    // Remaining: need fixed minute + hour
    if !minute.chars().all(|c| c.is_ascii_digit())
        || !hour.chars().all(|c| c.is_ascii_digit())
    {
        return cron.to_string();
    }
    let m: u32 = minute.parse().unwrap_or(0);
    let h: u32 = hour.parse().unwrap_or(0);

    // Daily: M H * * *
    if day_of_month == "*" && month == "*" && day_of_week == "*" {
        return format!("Every day at {}", format_local_time(m, h));
    }

    // Specific day of week: M H * * D
    if day_of_month == "*"
        && month == "*"
        && day_of_week.len() == 1
        && day_of_week.chars().all(|c| c.is_ascii_digit())
    {
        let day_idx = day_of_week.parse::<usize>().unwrap_or(0) % 7;
        return format!(
            "Every {} at {}",
            DAY_NAMES[day_idx],
            format_local_time(m, h)
        );
    }

    // Weekdays: M H * * 1-5
    if day_of_month == "*" && month == "*" && day_of_week == "1-5" {
        return format!("Weekdays at {}", format_local_time(m, h));
    }

    cron.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_basic() {
        let f = parse_cron_expression("* * * * *").unwrap();
        assert_eq!(f.minute.len(), 60);
        assert_eq!(f.hour.len(), 24);
        assert_eq!(f.day_of_month.len(), 31);
        assert_eq!(f.month.len(), 12);
        assert_eq!(f.day_of_week.len(), 7);
    }

    #[test]
    fn test_parse_specific() {
        let f = parse_cron_expression("30 14 * * *").unwrap();
        assert_eq!(f.minute, vec![30]);
        assert_eq!(f.hour, vec![14]);
    }

    #[test]
    fn test_parse_step() {
        let f = parse_cron_expression("*/5 * * * *").unwrap();
        assert_eq!(f.minute, vec![0, 5, 10, 15, 20, 25, 30, 35, 40, 45, 50, 55]);
    }

    #[test]
    fn test_parse_range() {
        let f = parse_cron_expression("0 9 * * 1-5").unwrap();
        assert_eq!(f.day_of_week, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn test_parse_list() {
        let f = parse_cron_expression("0,15,30,45 * * * *").unwrap();
        assert_eq!(f.minute, vec![0, 15, 30, 45]);
    }

    #[test]
    fn test_parse_dow_7_alias() {
        let f = parse_cron_expression("0 0 * * 7").unwrap();
        assert_eq!(f.day_of_week, vec![0]); // 7 → 0 (Sunday)
    }

    #[test]
    fn test_parse_dow_range_with_7() {
        let f = parse_cron_expression("0 0 * * 5-7").unwrap();
        assert_eq!(f.day_of_week, vec![0, 5, 6]); // 7→0
    }

    #[test]
    fn test_parse_invalid() {
        assert!(parse_cron_expression("").is_none());
        assert!(parse_cron_expression("* * *").is_none());
        assert!(parse_cron_expression("60 * * * *").is_none());
        assert!(parse_cron_expression("* 25 * * *").is_none());
        assert!(parse_cron_expression("* * 0 * *").is_none());
        assert!(parse_cron_expression("* * * 13 *").is_none());
        assert!(parse_cron_expression("* * * * 8").is_none());
    }

    #[test]
    fn test_parse_range_step() {
        let f = parse_cron_expression("0-30/10 * * * *").unwrap();
        assert_eq!(f.minute, vec![0, 10, 20, 30]);
    }

    #[test]
    fn test_compute_next_simple() {
        let fields = parse_cron_expression("30 14 * * *").unwrap();
        // From 14:00 → next is 14:30 same day
        let from = NaiveDate::from_ymd_opt(2024, 6, 15)
            .unwrap()
            .and_hms_opt(14, 0, 0)
            .unwrap();
        let next = compute_next_cron_run(&fields, from).unwrap();
        assert_eq!(next.hour(), 14);
        assert_eq!(next.minute(), 30);
        assert_eq!(next.day(), 15);
    }

    #[test]
    fn test_compute_next_rolls_day() {
        let fields = parse_cron_expression("30 14 * * *").unwrap();
        // From 14:30 → next is 14:30 next day
        let from = NaiveDate::from_ymd_opt(2024, 6, 15)
            .unwrap()
            .and_hms_opt(14, 30, 0)
            .unwrap();
        let next = compute_next_cron_run(&fields, from).unwrap();
        assert_eq!(next.day(), 16);
        assert_eq!(next.hour(), 14);
        assert_eq!(next.minute(), 30);
    }

    #[test]
    fn test_compute_next_every_5min() {
        let fields = parse_cron_expression("*/5 * * * *").unwrap();
        let from = NaiveDate::from_ymd_opt(2024, 6, 15)
            .unwrap()
            .and_hms_opt(10, 3, 0)
            .unwrap();
        let next = compute_next_cron_run(&fields, from).unwrap();
        assert_eq!(next.hour(), 10);
        assert_eq!(next.minute(), 5);
    }

    #[test]
    fn test_compute_next_weekday() {
        let fields = parse_cron_expression("0 9 * * 1").unwrap(); // Monday
        // 2024-06-15 is Saturday
        let from = NaiveDate::from_ymd_opt(2024, 6, 15)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap();
        let next = compute_next_cron_run(&fields, from).unwrap();
        assert_eq!(next.weekday(), chrono::Weekday::Mon);
        assert_eq!(next.day(), 17); // next Monday
    }

    #[test]
    fn test_compute_next_month_roll() {
        let fields = parse_cron_expression("0 0 1 * *").unwrap(); // 1st of each month
        let from = NaiveDate::from_ymd_opt(2024, 6, 2)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let next = compute_next_cron_run(&fields, from).unwrap();
        assert_eq!(next.month(), 7);
        assert_eq!(next.day(), 1);
    }

    #[test]
    fn test_compute_next_dom_or_dow() {
        // When both dom and dow are constrained, either matches (OR semantics)
        let fields = parse_cron_expression("0 0 15 * 1").unwrap(); // 15th OR Monday
        let from = NaiveDate::from_ymd_opt(2024, 6, 14)
            .unwrap()
            .and_hms_opt(1, 0, 0)
            .unwrap();
        let next = compute_next_cron_run(&fields, from).unwrap();
        // 2024-06-15 is Saturday, 2024-06-17 is Monday → 15th comes first
        assert_eq!(next.day(), 15);
    }

    #[test]
    fn test_cron_to_human_every_minute() {
        assert_eq!(cron_to_human("*/1 * * * *"), "Every minute");
        assert_eq!(cron_to_human("*/5 * * * *"), "Every 5 minutes");
    }

    #[test]
    fn test_cron_to_human_every_hour() {
        assert_eq!(cron_to_human("0 * * * *"), "Every hour");
        assert_eq!(cron_to_human("15 * * * *"), "Every hour at :15");
    }

    #[test]
    fn test_cron_to_human_daily() {
        assert_eq!(cron_to_human("30 14 * * *"), "Every day at 2:30 PM");
        assert_eq!(cron_to_human("0 9 * * *"), "Every day at 9:00 AM");
    }

    #[test]
    fn test_cron_to_human_weekday() {
        assert_eq!(cron_to_human("0 9 * * 1"), "Every Monday at 9:00 AM");
    }

    #[test]
    fn test_cron_to_human_weekdays() {
        assert_eq!(cron_to_human("0 9 * * 1-5"), "Weekdays at 9:00 AM");
    }

    #[test]
    fn test_cron_to_human_passthrough() {
        assert_eq!(cron_to_human("*/5 9-17 * * 1-5"), "*/5 9-17 * * 1-5");
    }

    #[test]
    fn test_next_cron_run_ms() {
        // Use a known cron that fires every minute — must get something
        let now_ms = chrono::Utc::now().timestamp_millis();
        let next = next_cron_run_ms("* * * * *", now_ms);
        assert!(next.is_some());
        assert!(next.unwrap() > now_ms);
    }

    #[test]
    fn test_every_n_hours() {
        let f = parse_cron_expression("0 */2 * * *").unwrap();
        assert_eq!(f.hour, vec![0, 2, 4, 6, 8, 10, 12, 14, 16, 18, 20, 22]);
    }

    #[test]
    fn test_cron_to_human_every_n_hours() {
        assert_eq!(cron_to_human("0 */2 * * *"), "Every 2 hours");
        assert_eq!(cron_to_human("15 */3 * * *"), "Every 3 hours at :15");
    }
}
