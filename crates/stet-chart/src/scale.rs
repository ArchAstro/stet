//! Turning values into positions, and choosing the ticks an axis shows.

use serde_json::Value;

/// A continuous scale from `domain` to `range`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Linear {
    pub domain: (f64, f64),
    pub range: (f32, f32),
}

impl Linear {
    pub fn at(&self, value: f64) -> f32 {
        let (d0, d1) = self.domain;
        let t = if d1 == d0 { 0.5 } else { (value - d0) / (d1 - d0) };
        self.range.0 + (self.range.1 - self.range.0) * t as f32
    }
}

/// Round numbers that cover `lo..hi` in about `count` steps: the domain grows
/// to the ticks at either end.
pub fn nice(lo: f64, hi: f64, count: usize) -> (f64, f64, f64) {
    let (lo, hi) = if lo == hi {
        let pad = if lo == 0.0 { 1.0 } else { lo.abs() * 0.1 };
        (lo - pad, hi + pad)
    } else {
        (lo, hi)
    };
    let rough = (hi - lo) / count.max(1) as f64;
    let power = 10f64.powf(rough.log10().floor());
    let step = [1.0, 2.0, 2.5, 5.0, 10.0]
        .iter()
        .map(|unit| unit * power)
        .find(|step| *step >= rough)
        .unwrap_or(10.0 * power);
    ((lo / step).floor() * step, (hi / step).ceil() * step, step)
}

/// The values from `lo` to `hi` by `step`.
pub fn ticks(lo: f64, hi: f64, step: f64) -> Vec<f64> {
    let count = ((hi - lo) / step).round() as usize;
    (0..=count.min(200)).map(|i| lo + step * i as f64).collect()
}

/// A number as an axis or readout shows it: as many decimals as `step`
/// needs, and thousands and millions shortened.
pub fn number(value: f64, step: f64) -> String {
    let big = value.abs().max(step.abs());
    if big >= 1e9 {
        return format!("{}B", number(value / 1e9, step / 1e9));
    }
    if big >= 1e6 {
        return format!("{}M", number(value / 1e6, step / 1e6));
    }
    if big >= 1e4 {
        return format!("{}k", number(value / 1e3, step / 1e3));
    }
    // As many decimals as the step has: 2.5 needs one, 0.25 two.
    let decimals = if step > 0.0 {
        (0..6)
            .find(|d| {
                let scaled = step * 10f64.powi(*d);
                (scaled - scaled.round()).abs() < 1e-6 * scaled.max(1.0)
            })
            .unwrap_or(6) as usize
    } else {
        2
    };
    let text = format!("{value:.decimals$}");
    if text.starts_with('-') && text.trim_start_matches(['-', '0', '.']).is_empty() {
        return text[1..].to_string();
    }
    text
}

/// A value read off a chart, with the precision it was written in.
pub fn readout(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        return format!("{value:.0}");
    }
    let text = format!("{value:.4}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

// --- Time -----------------------------------------------------------------

const MINUTE: f64 = 60_000.0;
const HOUR: f64 = 60.0 * MINUTE;
const DAY: f64 = 24.0 * HOUR;

/// Milliseconds since 1970 for a date or time value: a number is taken as
/// milliseconds already; text as ISO 8601 (`2024-03-01`, `2024-03-01T09:30`,
/// with seconds, fractions and an offset or `Z` if it has them).
pub fn time(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => parse_iso(text),
        _ => None,
    }
}

fn parse_iso(text: &str) -> Option<f64> {
    let text = text.trim();
    let (date, rest) = text.split_at(text.find(['T', ' ']).unwrap_or(text.len()));
    let mut parts = date.splitn(3, '-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: u32 = parts.next().map_or(Some(1), |part| part.parse().ok())?;
    let day: u32 = parts.next().map_or(Some(1), |part| part.parse().ok())?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let mut ms = days_from_civil(year, month, day) as f64 * DAY;
    let rest = rest.trim_start_matches(['T', ' ']);
    if rest.is_empty() {
        return Some(ms);
    }
    // The clock, then an offset: `09:30:05.250+02:00`, `09:30Z`.
    let split = rest.find(['Z', 'z', '+', '-']).unwrap_or(rest.len());
    let (clock, zone) = rest.split_at(split);
    let mut fields = clock.split(':');
    let hours: f64 = fields.next()?.parse().ok()?;
    let minutes: f64 = fields.next().map_or(Some(0.0), |field| field.parse().ok())?;
    let seconds: f64 = fields.next().map_or(Some(0.0), |field| field.parse().ok())?;
    ms += hours * HOUR + minutes * MINUTE + seconds * 1000.0;
    if let Some(sign) = zone.chars().next().filter(|c| *c == '+' || *c == '-') {
        let offset = zone[1..].replace(':', "");
        let hours: f64 = offset.get(..2)?.parse().ok()?;
        let minutes: f64 = offset.get(2..4).map_or(Some(0.0), |field| field.parse().ok())?;
        let offset = hours * HOUR + minutes * MINUTE;
        ms -= if sign == '+' { offset } else { -offset };
    }
    Some(ms)
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let month = month as i64;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date for days since 1970: `(year, month, day)`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

#[derive(Clone, Copy, Debug, PartialEq)]
enum Unit {
    Years(i64),
    Months(u32),
    Days(i64),
    Millis(f64),
}

/// Ticks for a time axis over `lo..hi` (ms), at calendar boundaries, with
/// their labels.
pub fn time_ticks(lo: f64, hi: f64, count: usize) -> Vec<(f64, String)> {
    let span = (hi - lo).max(1.0);
    let rough = span / count.max(1) as f64;
    let unit = if rough >= 300.0 * DAY {
        Unit::Years(((rough / (365.0 * DAY)).ceil() as i64).max(1))
    } else if rough >= 25.0 * DAY {
        Unit::Months(
            [1, 2, 3, 6]
                .into_iter()
                .find(|n| *n as f64 * 38.0 * DAY >= rough)
                .unwrap_or(6),
        )
    } else if rough >= DAY * 0.6 {
        Unit::Days(
            [1, 2, 7, 14]
                .into_iter()
                .find(|n| *n as f64 * 1.25 * DAY >= rough)
                .unwrap_or(14),
        )
    } else {
        let steps = [
            1000.0,
            5000.0,
            15000.0,
            MINUTE,
            5.0 * MINUTE,
            15.0 * MINUTE,
            HOUR,
            3.0 * HOUR,
            6.0 * HOUR,
            12.0 * HOUR,
        ];
        Unit::Millis(steps.into_iter().find(|step| *step >= rough).unwrap_or(12.0 * HOUR))
    };
    let day_of = |ms: f64| (ms / DAY).floor() as i64;
    let mut ticks = Vec::new();
    match unit {
        Unit::Years(step) => {
            let (first, ..) = civil_from_days(day_of(lo));
            let mut year = first - first.rem_euclid(step);
            loop {
                let at = days_from_civil(year, 1, 1) as f64 * DAY;
                if at > hi {
                    break;
                }
                if at >= lo {
                    ticks.push((at, year.to_string()));
                }
                year += step;
            }
        }
        Unit::Months(step) => {
            let (mut year, mut month, _) = civil_from_days(day_of(lo));
            month -= (month - 1) % step;
            loop {
                let at = days_from_civil(year, month, 1) as f64 * DAY;
                if at > hi {
                    break;
                }
                if at >= lo {
                    let name = MONTHS[month as usize - 1];
                    let label = if month == 1 || ticks.is_empty() {
                        format!("{name} {year}")
                    } else {
                        name.to_string()
                    };
                    ticks.push((at, label));
                }
                month += step;
                if month > 12 {
                    month -= 12;
                    year += 1;
                }
            }
        }
        Unit::Days(step) => {
            let mut day = day_of(lo);
            if step == 7 || step == 14 {
                // Weeks start on Monday; 1970-01-01 was a Thursday.
                day -= (day + 3).rem_euclid(7);
            }
            while day as f64 * DAY <= hi {
                let at = day as f64 * DAY;
                if at >= lo {
                    let (_, month, date) = civil_from_days(day);
                    ticks.push((at, format!("{} {date}", MONTHS[month as usize - 1])));
                }
                day += step;
            }
        }
        Unit::Millis(step) => {
            let mut at = (lo / step).ceil() * step;
            while at <= hi {
                ticks.push((at, clock(at, step < MINUTE)));
                at += step;
            }
        }
    }
    ticks
}

fn clock(ms: f64, seconds: bool) -> String {
    let of_day = ms.rem_euclid(DAY);
    let (h, m, s) = (
        (of_day / HOUR) as u32,
        (of_day % HOUR / MINUTE) as u32,
        (of_day % MINUTE / 1000.0) as u32,
    );
    if seconds {
        format!("{h:02}:{m:02}:{s:02}")
    } else {
        format!("{h:02}:{m:02}")
    }
}

/// A moment as a readout shows it: the date, and the time if it has one.
pub fn moment(ms: f64) -> String {
    let day = (ms / DAY).floor() as i64;
    let (year, month, date) = civil_from_days(day);
    let mut text = format!("{year}-{month:02}-{date:02}");
    if ms.rem_euclid(DAY) != 0.0 {
        text.push(' ');
        text.push_str(&clock(ms, ms.rem_euclid(MINUTE) != 0.0));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn nice_domains_land_on_round_steps() {
        assert_eq!(nice(0.0, 11.5, 5), (0.0, 12.5, 2.5));
        assert_eq!(nice(3.0, 97.0, 5), (0.0, 100.0, 20.0));
        assert_eq!(nice(-0.3, 0.7, 5), (-0.4, 0.8, 0.2));
        let (lo, hi, _) = nice(5.0, 5.0, 5);
        assert!(lo < 5.0 && hi > 5.0);
    }

    #[test]
    fn numbers_read_like_an_axis() {
        assert_eq!(number(2.5, 0.5), "2.5");
        assert_eq!(number(10.0, 2.5), "10.0");
        assert_eq!(number(40.0, 20.0), "40");
        assert_eq!(number(25_000.0, 5000.0), "25k");
        assert_eq!(number(1_500_000.0, 500_000.0), "1.5M");
        assert_eq!(number(-0.0001, 0.2), "0.0");
        assert_eq!(readout(3.10000001), "3.1");
        assert_eq!(readout(42.0), "42");
    }

    #[test]
    fn dates_and_times_parse() {
        assert_eq!(time(&json!("1970-01-02")), Some(DAY));
        assert_eq!(time(&json!("2024-03-01")), Some(19_783.0 * DAY));
        assert_eq!(time(&json!("2024-03-01T01:30:00Z")), Some(19_783.0 * DAY + 1.5 * HOUR));
        assert_eq!(
            time(&json!("2024-03-01T03:30+02:00")),
            Some(19_783.0 * DAY + 1.5 * HOUR)
        );
        assert_eq!(time(&json!(1000)), Some(1000.0));
        assert_eq!(time(&json!("W27")), None);
        assert_eq!(civil_from_days(19_783), (2024, 3, 1));
        assert_eq!(moment(19_783.0 * DAY + 1.5 * HOUR), "2024-03-01 01:30");
    }

    #[test]
    fn time_ticks_fall_on_calendar_boundaries() {
        let at = |text: &str| time(&json!(text)).unwrap();
        let months = time_ticks(at("2024-01-15"), at("2024-07-20"), 6);
        let labels: Vec<&str> = months.iter().map(|(_, label)| label.as_str()).collect();
        assert_eq!(labels, ["Feb 2024", "Mar", "Apr", "May", "Jun", "Jul"]);
        let years = time_ticks(at("2015-06-01"), at("2024-06-01"), 5);
        assert_eq!(years[0].1, "2016");
        let days = time_ticks(at("2024-03-01"), at("2024-03-04"), 4);
        assert_eq!(days[0].1, "Mar 1");
        let hours = time_ticks(at("2024-03-01T00:00Z"), at("2024-03-01T12:00Z"), 4);
        assert_eq!(hours[1].1, "03:00");
    }
}
