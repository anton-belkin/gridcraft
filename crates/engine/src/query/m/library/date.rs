//! `Date.*`, `DateTime.*`, `Duration.*` and the `#date`, `#datetime`, `#datetimezone`, `#time` and
//! `#duration` constructors.

use super::super::eval::Ev;
use super::super::value::{MError, MResult, MValue, days, ymd};
use super::{Args, cant, show};

pub fn call(_ev: &mut Ev<'_, '_>, name: &str, a: &Args<'_>) -> MResult<Option<MValue>> {
    let r = match name {
        "Date.Year" | "Date.Month" | "Date.Day" => match day_of(a.at(0))? {
            None => MValue::Null,
            Some(d) => {
                let (y, m, dd) = ymd(d);
                MValue::Number(f64::from(match name {
                    "Date.Year" => y,
                    "Date.Month" => m as i32,
                    _ => dd as i32,
                }))
            }
        },
        "Date.AddDays" => {
            let n = a.num(1)?;
            if !n.is_finite() || n.fract() != 0.0 || n.abs() > 4_000_000.0 {
                return Err(cant(a.at(1), "Number"));
            }
            match a.at(0) {
                MValue::Null => MValue::Null,
                MValue::Date(d) => MValue::Date(checked_day(d.saturating_add(n as i64))?),
                MValue::DateTime(d) => MValue::DateTime(d + n),
                MValue::DateTimeZone(d, o) => MValue::DateTimeZone(d + n, *o),
                other => return Err(cant(other, "Date")),
            }
        }
        "DateTime.Date" => match day_of(a.at(0))? {
            None => MValue::Null,
            Some(d) => MValue::Date(d),
        },
        "DateTime.Time" => match a.at(0) {
            MValue::Null => MValue::Null,
            MValue::DateTime(d) | MValue::DateTimeZone(d, _) => MValue::Time(d.rem_euclid(1.0)),
            MValue::Time(t) => MValue::Time(*t),
            other => return Err(cant(other, "DateTime")),
        },
        "DateTime.LocalNow" => MValue::DateTime(gridcraft_core::date::unix_now_ms().unwrap_or(0.0) / 86_400_000.0),
        "#date" => MValue::Date(days(whole(a, 0)?, whole(a, 1)?, whole(a, 2)?)?),
        "#datetime" => MValue::DateTime(days(whole(a, 0)?, whole(a, 1)?, whole(a, 2)?)? as f64 + time_of(a, 3)?),
        "#datetimezone" => {
            let (oh, om) = (whole(a, 6)?, whole(a, 7)?);
            if !(-14..=14).contains(&oh) || !(-59..=59).contains(&om) {
                return Err(MError::expr("The time zone offset is out of range."));
            }
            MValue::DateTimeZone(days(whole(a, 0)?, whole(a, 1)?, whole(a, 2)?)? as f64 + time_of(a, 3)?, (oh * 60 + om) as i32)
        }
        "#time" => MValue::Time(time_of(a, 0)?),
        "#duration" => MValue::Duration(a.num(0)? + a.num(1)? / 24.0 + a.num(2)? / 1440.0 + a.num(3)? / 86_400.0),
        _ => return Ok(None),
    };
    Ok(Some(r))
}

pub fn whole(a: &Args<'_>, i: usize) -> MResult<i64> {
    let n = a.num(i)?;
    if n.fract() != 0.0 || !n.is_finite() || n.abs() > 1.0e9 {
        return Err(MError::expr(format!("{} isn't a whole number in range.", show(a.at(i)))));
    }
    Ok(n as i64)
}

pub fn time_of(a: &Args<'_>, i: usize) -> MResult<f64> {
    let (h, m, s) = (a.num(i)?, a.num(i + 1)?, a.num(i + 2)?);
    if !(0.0..24.0).contains(&h) || !(0.0..60.0).contains(&m) || !(0.0..60.0).contains(&s) || h.fract() != 0.0 || m.fract() != 0.0 {
        return Err(MError::expr(format!("#time({h}, {m}, {s}) isn't a valid time.")));
    }
    Ok(h / 24.0 + m / 1440.0 + s / 86_400.0)
}

pub fn day_of(v: &MValue) -> MResult<Option<i64>> {
    match v {
        MValue::Null => Ok(None),
        MValue::Date(d) => Ok(Some(*d)),
        MValue::DateTime(d) | MValue::DateTimeZone(d, _) => Ok(Some(d.floor() as i64)),
        other => Err(cant(other, "Date")),
    }
}

pub fn checked_day(d: i64) -> MResult<i64> {
    let (y, _, _) = ymd(d);
    if (1..=9999).contains(&y) { Ok(d) } else { Err(MError::expr("The date is out of range.")) }
}
