//! Scalar and list functions: conversions (`Text.From`, `Number.From`, Change Type), `Text.*`,
//! `Number.*`, `Date.*`, `List.*`, `Record.*`, and the splitters, combiners, replacers and
//! comparers that table functions take. Text positions count UTF-16 code units, as M does.

use std::rc::Rc;

use gridcraft_core::DateSystem;

use super::super::eval::{Ev, MAX_LIST, limit};
use super::super::value::{
    Key, MError, MRecord, MResult, MType, MValue, date_text, days, duration_text, equal, key, order, sort_order, time_text, ymd,
};
use super::{Args, cant, show};
use crate::query::types::{Culture, DateOrder, parse_datetime, parse_number, parse_time};

/// Days from 1899-12-30 (OLE Automation dates, which `Number.From(date)` gives) to 1970-01-01.
const OLE_1970: f64 = 25_569.0;

pub fn call(ev: &mut Ev<'_, '_>, name: &str, a: &Args<'_>) -> MResult<Option<MValue>> {
    let r = match name {
        // conversions
        "Text.From" => match a.at(0) {
            MValue::Null => MValue::Null,
            v => MValue::text(text_of(v, a.opt_text(1)?.as_deref()).ok_or_else(|| cant(v, "Text"))?),
        },
        "Number.From" | "Number.FromText" => {
            if name == "Number.FromText" && !matches!(a.at(0), MValue::Text(_) | MValue::Null) {
                return Err(cant(a.at(0), "Text"));
            }
            raise(convert(a.at(0), &MType::prim("number"), a.opt_text(1)?.as_deref(), ev.sys))?
        }
        "Date.From" => raise(convert(a.at(0), &MType::prim("date"), a.opt_text(1)?.as_deref(), ev.sys))?,
        "DateTime.From" => raise(convert(a.at(0), &MType::prim("datetime"), a.opt_text(1)?.as_deref(), ev.sys))?,
        "Logical.From" => raise(convert(a.at(0), &MType::prim("logical"), None, ev.sys))?,
        // text
        "Text.Upper" => map_text(a.at(0), |s| s.to_uppercase())?,
        "Text.Lower" => map_text(a.at(0), |s| s.to_lowercase())?,
        "Text.Proper" => map_text(a.at(0), proper)?,
        "Text.Trim" => {
            let chars: Option<Vec<char>> = match a.at(1) {
                MValue::Null => None,
                MValue::Text(t) => Some(t.chars().collect()),
                MValue::List(l) => Some(l.iter().filter_map(|x| if let MValue::Text(t) = x { t.chars().next() } else { None }).collect()),
                other => return Err(cant(other, "Text")),
            };
            map_text(a.at(0), |s| match &chars {
                None => s.trim().to_string(),
                Some(cs) => s.trim_matches(|c| cs.contains(&c)).to_string(),
            })?
        }
        "Text.Clean" => map_text(a.at(0), |s| s.chars().filter(|c| !c.is_control()).collect())?,
        "Text.Contains" | "Text.StartsWith" | "Text.EndsWith" => {
            let (MValue::Text(t), sub) = (a.at(0), a.text(1)?) else {
                return if a.at(0).is_null() { Ok(Some(MValue::Null)) } else { Err(cant(a.at(0), "Text")) };
            };
            let fold = a.has(2) && is_ignore_case(a.at(2));
            let (t, sub) = if fold { (t.to_lowercase(), sub.to_lowercase()) } else { (t.to_string(), sub.to_string()) };
            MValue::Logical(match name {
                "Text.Contains" => t.contains(&sub),
                "Text.StartsWith" => t.starts_with(&sub),
                _ => t.ends_with(&sub),
            })
        }
        "Text.Length" => match a.at(0) {
            MValue::Null => MValue::Null,
            MValue::Text(t) => MValue::Number(t.encode_utf16().count() as f64),
            other => return Err(cant(other, "Text")),
        },
        "Text.Replace" => match a.at(0) {
            MValue::Null => MValue::Null,
            v => replace_text(v, &MValue::Text(a.text(1)?), a.at(2))?,
        },
        "Text.Combine" => {
            let l = a.list(0)?;
            let sep = a.opt_text(1)?.unwrap_or_else(|| Rc::from(""));
            let mut parts = Vec::with_capacity(l.len());
            for x in l.iter() {
                match ev.force(x.clone())? {
                    MValue::Null => {}
                    MValue::Text(t) => parts.push(t),
                    other => return Err(cant(&other, "Text")),
                }
            }
            long_text(parts.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(&sep))?
        }
        "Text.Split" => {
            let t = a.text(0)?;
            let sep = a.text(1)?;
            if sep.is_empty() {
                MValue::list(t.chars().map(|c| MValue::text(c.to_string())).collect())
            } else {
                MValue::list(t.split(&*sep).map(MValue::text).collect())
            }
        }
        "Text.Start" | "Text.End" => match a.at(0) {
            MValue::Null => MValue::Null,
            MValue::Text(t) => {
                let u: Vec<u16> = t.encode_utf16().collect();
                let n = a.count(1)?.min(u.len());
                let slice = if name == "Text.Start" { u.get(..n) } else { u.get(u.len() - n..) };
                MValue::text(String::from_utf16_lossy(slice.unwrap_or(&[])))
            }
            other => return Err(cant(other, "Text")),
        },
        "Text.Middle" => match a.at(0) {
            MValue::Null => MValue::Null,
            MValue::Text(t) => {
                let u: Vec<u16> = t.encode_utf16().collect();
                let start = a.count(1)?.min(u.len());
                let count = if a.has(2) { a.count(2)? } else { usize::MAX };
                let end = start.saturating_add(count).min(u.len());
                MValue::text(String::from_utf16_lossy(u.get(start..end).unwrap_or(&[])))
            }
            other => return Err(cant(other, "Text")),
        },
        "Text.BeforeDelimiter" | "Text.AfterDelimiter" => match a.at(0) {
            MValue::Null => MValue::Null,
            MValue::Text(t) => {
                let d = a.text(1)?;
                let (n, from_end) = delimiter_index(a.at(2))?;
                match find_nth(t, &d, n, from_end) {
                    Some(p) if name == "Text.BeforeDelimiter" => MValue::text(t.get(..p).unwrap_or("")),
                    Some(p) => MValue::text(t.get(p + d.len()..).unwrap_or("")),
                    None if name == "Text.BeforeDelimiter" => MValue::Text(t.clone()),
                    None => MValue::text(""),
                }
            }
            other => return Err(cant(other, "Text")),
        },
        "Text.BetweenDelimiters" => match a.at(0) {
            MValue::Null => MValue::Null,
            MValue::Text(t) => {
                let (sd, ed) = (a.text(1)?, a.text(2)?);
                let (sn, s_end) = delimiter_index(a.at(3))?;
                let (en, _) = delimiter_index(a.at(4))?;
                match find_nth(t, &sd, sn, s_end) {
                    None => MValue::text(""),
                    Some(p) => {
                        let rest = t.get(p + sd.len()..).unwrap_or("");
                        match find_nth(rest, &ed, en, false) {
                            Some(q) => MValue::text(rest.get(..q).unwrap_or("")),
                            None => MValue::text(rest),
                        }
                    }
                }
            }
            other => return Err(cant(other, "Text")),
        },
        // numbers
        "Number.Round" | "Number.RoundUp" | "Number.RoundDown" => match a.at(0) {
            MValue::Null => MValue::Null,
            MValue::Number(n) => {
                let digits = a.int(1, 0)?.clamp(-15, 15) as i32;
                let mode = match name {
                    "Number.RoundUp" => 0,
                    "Number.RoundDown" => 1,
                    _ => a.int(2, 4)?,
                };
                MValue::Number(round(*n, digits, mode))
            }
            other => return Err(cant(other, "Number")),
        },
        "Number.Abs" => num1(a.at(0), f64::abs)?,
        "Number.Sqrt" => num1(a.at(0), f64::sqrt)?,
        "Number.Power" => match (a.at(0), a.at(1)) {
            (MValue::Null, _) | (_, MValue::Null) => MValue::Null,
            _ => MValue::Number(a.num(0)?.powf(a.num(1)?)),
        },
        "Number.Mod" | "Number.IntegerDivide" => match (a.at(0), a.at(1)) {
            (MValue::Null, _) | (_, MValue::Null) => MValue::Null,
            _ => {
                let (x, y) = (a.num(0)?, a.num(1)?);
                if y == 0.0 {
                    return Err(MError::expr("Division by zero."));
                }
                MValue::Number(if name == "Number.Mod" { x % y } else { (x / y).trunc() })
            }
        },
        // dates
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
        // lists
        "List.Sum" | "List.Average" => {
            let items = forced(ev, &a.list(0)?)?;
            let vals: Vec<&MValue> = items.iter().filter(|v| !v.is_null()).collect();
            if vals.is_empty() {
                MValue::Null
            } else if vals.iter().all(|v| matches!(v, MValue::Number(_))) {
                let s: f64 = vals.iter().map(|v| if let MValue::Number(n) = v { *n } else { 0.0 }).sum();
                MValue::Number(if name == "List.Sum" { s } else { s / vals.len() as f64 })
            } else if vals.iter().all(|v| matches!(v, MValue::Duration(_))) {
                let s: f64 = vals.iter().map(|v| if let MValue::Duration(n) = v { *n } else { 0.0 }).sum();
                MValue::Duration(if name == "List.Sum" { s } else { s / vals.len() as f64 })
            } else {
                let bad = vals.iter().find(|v| !matches!(v, MValue::Number(_))).map(|v| (*v).clone()).unwrap_or(MValue::Null);
                return Err(cant(&bad, "Number"));
            }
        }
        "List.Median" => {
            let items = forced(ev, &a.list(0)?)?;
            let mut nums = Vec::with_capacity(items.len());
            for v in items.iter().filter(|v| !v.is_null()) {
                match v {
                    MValue::Number(n) => nums.push(*n),
                    other => return Err(cant(other, "Number")),
                }
            }
            nums.sort_by(f64::total_cmp);
            let n = nums.len();
            match n {
                0 => MValue::Null,
                _ if n % 2 == 1 => MValue::Number(nums.get(n / 2).copied().unwrap_or(0.0)),
                _ => MValue::Number((nums.get(n / 2 - 1).copied().unwrap_or(0.0) + nums.get(n / 2).copied().unwrap_or(0.0)) / 2.0),
            }
        }
        "List.Min" | "List.Max" => {
            let items = forced(ev, &a.list(0)?)?;
            let mut best: Option<MValue> = None;
            for v in items.into_iter().filter(|v| !v.is_null()) {
                best = Some(match best {
                    None => v,
                    Some(b) => {
                        let o = order(&v, &b).ok_or_else(|| MError::expr(format!("We cannot compare {} and {}.", show(&v), show(&b))))?;
                        if (name == "List.Min" && o.is_lt()) || (name == "List.Max" && o.is_gt()) { v } else { b }
                    }
                });
            }
            best.unwrap_or_else(|| a.at(1).clone())
        }
        "List.Count" => MValue::Number(a.list(0)?.len() as f64),
        "List.NonNullCount" => MValue::Number(forced(ev, &a.list(0)?)?.iter().filter(|v| !v.is_null()).count() as f64),
        "List.Distinct" => {
            let fold = is_ignore_case(a.at(1));
            let mut seen = std::collections::HashSet::new();
            let mut out = Vec::new();
            for v in forced(ev, &a.list(0)?)? {
                if seen.insert(folded_key(&v, fold)) {
                    out.push(v);
                }
            }
            MValue::list(out)
        }
        "List.First" | "List.Last" => {
            let l = a.list(0)?;
            let x = if name == "List.First" { l.first() } else { l.last() };
            match x {
                Some(v) => ev.force(v.clone())?,
                None => a.at(1).clone(),
            }
        }
        "List.Select" => {
            let f = a.func(1)?;
            let mut out = Vec::new();
            for v in a.list(0)?.iter() {
                let v = ev.force(v.clone())?;
                if matches!(ev.call1(&f, v.clone())?, MValue::Logical(true)) {
                    out.push(v);
                }
            }
            MValue::list(out)
        }
        "List.Transform" => {
            let f = a.func(1)?;
            let l = a.list(0)?;
            let mut out = Vec::with_capacity(l.len());
            for v in l.iter() {
                let r = ev.force(v.clone()).and_then(|v| ev.call1(&f, v));
                out.push(super::table::held(r)?);
            }
            MValue::list(out)
        }
        "List.Contains" | "List.PositionOf" => {
            let fold = is_ignore_case(a.at(if name == "List.Contains" { 2 } else { 3 }));
            let want = folded(a.at(1), fold);
            let mut found = None;
            for (i, v) in a.list(0)?.iter().enumerate() {
                let v = ev.force(v.clone())?;
                if equal(&folded(&v, fold), &want) {
                    found = Some(i);
                    if name == "List.Contains" || !matches!(a.at(2), MValue::Number(n) if *n == 1.0) {
                        break;
                    }
                }
            }
            if name == "List.Contains" { MValue::Logical(found.is_some()) } else { MValue::Number(found.map(|i| i as f64).unwrap_or(-1.0)) }
        }
        "List.Combine" => {
            let mut out = Vec::new();
            for x in a.list(0)?.iter() {
                match ev.force(x.clone())? {
                    MValue::List(l) => {
                        if out.len().saturating_add(l.len()) > MAX_LIST {
                            return Err(limit(format!("A list can hold at most {MAX_LIST} items here.")));
                        }
                        out.extend(l.iter().cloned());
                    }
                    other => return Err(cant(&other, "List")),
                }
            }
            MValue::list(out)
        }
        "List.RemoveNulls" => MValue::list(forced(ev, &a.list(0)?)?.into_iter().filter(|v| !v.is_null()).collect()),
        "List.Sort" => {
            let mut items = forced(ev, &a.list(0)?)?;
            let desc = matches!(a.at(1), MValue::Number(n) if *n == 1.0);
            ev.tick(items.len() as u64 * 20)?;
            items.sort_by(|x, y| if desc { sort_order(y, x) } else { sort_order(x, y) });
            MValue::list(items)
        }
        "List.Reverse" => MValue::list(a.list(0)?.iter().rev().cloned().collect()),
        "List.Numbers" => {
            let start = a.num(0)?;
            let count = a.count(1)?;
            let step = if a.has(2) { a.num(2)? } else { 1.0 };
            if count > MAX_LIST {
                return Err(limit(format!("A list can hold at most {MAX_LIST} items here.")));
            }
            ev.tick(count as u64)?;
            MValue::list((0..count).map(|i| MValue::Number(start + i as f64 * step)).collect())
        }
        "List.Buffer" => MValue::List(a.list(0)?),
        "List.IsEmpty" => MValue::Logical(a.list(0)?.is_empty()),
        // records and errors
        "Record.Field" => {
            let r = a.record(0)?;
            let n = a.text(1)?;
            ev.field(&MValue::Record(r), &n, false)?
        }
        "Record.FieldNames" => MValue::list(a.record(0)?.names.iter().map(|n| MValue::Text(n.clone())).collect()),
        "Record.FieldValues" => MValue::list(a.record(0)?.values.clone()),
        "Record.HasFields" => {
            let r = a.record(0)?;
            MValue::Logical(super::names_of(a.at(1))?.iter().all(|n| r.get(n).is_some()))
        }
        "Error.Record" => MValue::record(MRecord::new(vec![
            ("Reason".into(), a.at(0).clone()),
            ("Message".into(), a.at(1).clone()),
            ("Detail".into(), a.at(2).clone()),
        ])),
        // splitters, combiners, replacers, comparers
        "Splitter.SplitTextByDelimiter"
        | "Splitter.SplitTextByEachDelimiter"
        | "Splitter.SplitTextByPositions"
        | "Splitter.SplitTextByLengths"
        | "Splitter.SplitByNothing"
        | "Combiner.CombineTextByDelimiter" => MValue::bound(&format!("{name}#apply"), a.v.clone()),
        "Splitter.SplitTextByDelimiter#apply" => split_apply(a, Split::Delimiter)?,
        "Splitter.SplitTextByEachDelimiter#apply" => split_apply(a, Split::Each)?,
        "Splitter.SplitTextByPositions#apply" => split_apply(a, Split::Positions)?,
        "Splitter.SplitTextByLengths#apply" => split_apply(a, Split::Lengths)?,
        "Splitter.SplitByNothing#apply" => MValue::list(vec![a.v.last().cloned().unwrap_or(MValue::Null)]),
        "Combiner.CombineTextByDelimiter#apply" => {
            let delim = a.text(0)?;
            let MValue::List(l) = a.v.last().cloned().unwrap_or(MValue::Null) else {
                return Err(cant(a.v.last().unwrap_or(&MValue::Null), "List"));
            };
            let mut parts = Vec::with_capacity(l.len());
            for v in l.iter() {
                match ev.force(v.clone())? {
                    MValue::Null => parts.push(String::new()),
                    MValue::Text(t) => parts.push(t.to_string()),
                    other => return Err(cant(&other, "Text")),
                }
            }
            long_text(parts.join(&delim))?
        }
        "Replacer.ReplaceText" => replace_text(a.at(0), a.at(1), a.at(2))?,
        "Replacer.ReplaceValue" => {
            if equal(a.at(0), a.at(1)) {
                a.at(2).clone()
            } else {
                a.at(0).clone()
            }
        }
        "Comparer.Ordinal" | "Comparer.OrdinalIgnoreCase" => {
            let fold = name == "Comparer.OrdinalIgnoreCase";
            let o = sort_order(&folded(a.at(0), fold), &folded(a.at(1), fold));
            MValue::Number(o as i8 as f64)
        }
        _ => return Ok(None),
    };
    Ok(Some(r))
}

// ---------------------------------------------------------------- conversions

/// A held conversion error raised as the function's error.
fn raise(v: MValue) -> MResult<MValue> {
    match v {
        MValue::Error(e) => Err((*e).clone()),
        v => Ok(v),
    }
}

pub fn number_text(n: f64) -> String {
    if n.is_nan() {
        "NaN".into()
    } else if n.is_infinite() {
        if n > 0.0 { "Infinity".into() } else { "-Infinity".into() }
    } else {
        gridcraft_core::number_to_text(n)
    }
}

fn is_dmy(culture: Option<&str>) -> bool {
    Culture::of(culture).order == DateOrder::Dmy
}

/// Text for a value as `Text.From` gives it (`None` for values that have no text form).
pub fn text_of(v: &MValue, culture: Option<&str>) -> Option<String> {
    let dmy = is_dmy(culture);
    Some(match v {
        MValue::Text(t) => t.to_string(),
        MValue::Number(n) => {
            let s = number_text(*n);
            if Culture::of(culture).decimal == ',' { s.replace('.', ",") } else { s }
        }
        MValue::Logical(b) => if *b { "true" } else { "false" }.to_string(),
        MValue::Date(d) => date_text(*d, dmy),
        MValue::DateTime(d) => format!("{} {}", date_text(d.floor() as i64, dmy), time_text(d.rem_euclid(1.0))),
        MValue::DateTimeZone(d, o) => {
            format!(
                "{} {} {}{:02}:{:02}",
                date_text(d.floor() as i64, dmy),
                time_text(d.rem_euclid(1.0)),
                if *o < 0 { '-' } else { '+' },
                o.abs() / 60,
                o.abs() % 60
            )
        }
        MValue::Time(t) => time_text(*t),
        MValue::Duration(d) => duration_text(*d),
        _ => return None,
    })
}

fn bad(kind: &str) -> MValue {
    MValue::error(MError::format(format!("We couldn't convert to {kind}.")))
}

fn ole_days(serial_1900: f64) -> f64 {
    serial_1900 - OLE_1970
}

/// A value converted to `ty` (Change Type, `Number.From`…); a value that can't be converted
/// becomes a held `DataFormat.Error`.
pub fn convert(v: &MValue, ty: &MType, culture: Option<&str>, _sys: DateSystem) -> MValue {
    if v.is_null() || matches!(v, MValue::Error(_)) {
        return v.clone();
    }
    let cul = Culture::of(culture);
    match ty.kind {
        "any" | "anynonnull" => v.clone(),
        "text" => match text_of(v, culture) {
            Some(s) => MValue::text(s),
            None => bad("Text"),
        },
        "number" => {
            let n = match v {
                MValue::Number(n) => Some(*n),
                MValue::Logical(b) => Some(if *b { 1.0 } else { 0.0 }),
                MValue::Text(t) => {
                    let t = t.trim();
                    if t.is_empty() {
                        return MValue::Null;
                    }
                    parse_number(t, &cul).or_else(|| parse_datetime(t, &cul, DateSystem::D1900).map(|(n, _)| n))
                }
                MValue::Date(d) => Some(*d as f64 + OLE_1970),
                MValue::DateTime(d) | MValue::DateTimeZone(d, _) => Some(d + OLE_1970),
                MValue::Time(t) | MValue::Duration(t) => Some(*t),
                _ => None,
            };
            match n {
                Some(n) => MValue::Number(match ty.facet {
                    Some("Int64.Type" | "Int32.Type" | "Int16.Type" | "Int8.Type" | "Byte.Type") => round(n, 0, 4),
                    Some("Currency.Type") => round(n, 4, 4),
                    _ => n,
                }),
                None => bad("Number"),
            }
        }
        "date" | "datetime" | "datetimezone" => {
            let d: Option<f64> = match v {
                MValue::Date(d) => Some(*d as f64),
                MValue::DateTime(d) | MValue::DateTimeZone(d, _) => Some(*d),
                MValue::Number(n) if n.is_finite() => Some(ole_days(*n)),
                MValue::Text(t) => {
                    let t = t.trim();
                    if t.is_empty() {
                        return MValue::Null;
                    }
                    parse_datetime(t, &cul, DateSystem::D1900).map(|(n, _)| ole_days(n))
                }
                _ => None,
            };
            match d.filter(|d| d.is_finite() && (-719_162.0..=2_932_896.0).contains(d)) {
                Some(d) => match ty.kind {
                    "date" => MValue::Date(d.floor() as i64),
                    "datetimezone" => match v {
                        MValue::DateTimeZone(..) => v.clone(),
                        _ => MValue::DateTimeZone(d, 0),
                    },
                    _ => MValue::DateTime(d),
                },
                None => MValue::error(MError::format(format!(
                    "We couldn't parse the input provided as a {} value.",
                    if ty.kind == "date" { "Date" } else { "DateTime" }
                ))),
            }
        }
        "time" => {
            let t = match v {
                MValue::Time(t) => Some(*t),
                MValue::DateTime(d) | MValue::DateTimeZone(d, _) => Some(d.rem_euclid(1.0)),
                MValue::Number(n) if (0.0..1.0).contains(n) => Some(*n),
                MValue::Text(s) => {
                    let s = s.trim();
                    if s.is_empty() {
                        return MValue::Null;
                    }
                    parse_time(s).or_else(|| parse_datetime(s, &cul, DateSystem::D1900).map(|(n, _)| n.rem_euclid(1.0)))
                }
                _ => None,
            };
            match t {
                Some(t) => MValue::Time(t),
                None => MValue::error(MError::format("We couldn't parse the input provided as a Time value.")),
            }
        }
        "duration" => {
            let d = match v {
                MValue::Duration(d) => Some(*d),
                MValue::Number(n) => Some(*n),
                MValue::Time(t) => Some(*t),
                MValue::Text(s) => parse_duration(s.trim()),
                _ => None,
            };
            match d {
                Some(d) => MValue::Duration(d),
                None => MValue::error(MError::format("We couldn't parse the input provided as a Duration value.")),
            }
        }
        "logical" => match v {
            MValue::Logical(_) => v.clone(),
            MValue::Number(n) => MValue::Logical(*n != 0.0),
            MValue::Text(t) if t.trim().eq_ignore_ascii_case("true") => MValue::Logical(true),
            MValue::Text(t) if t.trim().eq_ignore_ascii_case("false") => MValue::Logical(false),
            _ => bad("Logical"),
        },
        kind if kind == v.kind() => v.clone(),
        _ => MValue::error(MError::expr(format!("We cannot convert a value of type {} to type {}.", v.kind(), ty.kind))),
    }
}

/// `[-][d.]hh:mm[:ss[.fff]]`.
fn parse_duration(s: &str) -> Option<f64> {
    let (neg, s) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s),
    };
    let (days, rest) = match s.split_once('.') {
        Some((d, r)) if r.contains(':') && !d.contains(':') => (d.parse::<f64>().ok()?, r),
        _ => (0.0, s),
    };
    let mut parts = rest.split(':');
    let h: f64 = parts.next()?.parse().ok()?;
    let m: f64 = parts.next()?.parse().ok()?;
    let sec: f64 = parts.next().map(str::parse).transpose().ok()?.unwrap_or(0.0);
    if parts.next().is_some() {
        return None;
    }
    let d = days + h / 24.0 + m / 1440.0 + sec / 86_400.0;
    Some(if neg { -d } else { d })
}

/// `Number.Round`'s rounding modes: Up 0, Down 1, AwayFromZero 2, TowardZero 3, ToEven 4.
pub fn round(n: f64, digits: i32, mode: i64) -> f64 {
    if !n.is_finite() {
        return n;
    }
    let scale = 10f64.powi(digits);
    let x = n * scale;
    if !x.is_finite() {
        return n;
    }
    let r = match mode {
        0 => x.ceil(),
        1 => x.floor(),
        2 => x.round(),
        3 => x.trunc(),
        _ => {
            let r = x.round();
            if (x - x.trunc()).abs() == 0.5 && r % 2.0 != 0.0 { r - x.signum() } else { r }
        }
    };
    r / scale
}

fn num1(v: &MValue, f: impl Fn(f64) -> f64) -> MResult<MValue> {
    match v {
        MValue::Null => Ok(MValue::Null),
        MValue::Number(n) => Ok(MValue::Number(f(*n))),
        other => Err(cant(other, "Number")),
    }
}

fn whole(a: &Args<'_>, i: usize) -> MResult<i64> {
    let n = a.num(i)?;
    if n.fract() != 0.0 || !n.is_finite() || n.abs() > 1.0e9 {
        return Err(MError::expr(format!("{} isn't a whole number in range.", show(a.at(i)))));
    }
    Ok(n as i64)
}

fn time_of(a: &Args<'_>, i: usize) -> MResult<f64> {
    let (h, m, s) = (a.num(i)?, a.num(i + 1)?, a.num(i + 2)?);
    if !(0.0..24.0).contains(&h) || !(0.0..60.0).contains(&m) || !(0.0..60.0).contains(&s) || h.fract() != 0.0 || m.fract() != 0.0 {
        return Err(MError::expr(format!("#time({h}, {m}, {s}) isn't a valid time.")));
    }
    Ok(h / 24.0 + m / 1440.0 + s / 86_400.0)
}

fn day_of(v: &MValue) -> MResult<Option<i64>> {
    match v {
        MValue::Null => Ok(None),
        MValue::Date(d) => Ok(Some(*d)),
        MValue::DateTime(d) | MValue::DateTimeZone(d, _) => Ok(Some(d.floor() as i64)),
        other => Err(cant(other, "Date")),
    }
}

fn checked_day(d: i64) -> MResult<i64> {
    let (y, _, _) = ymd(d);
    if (1..=9999).contains(&y) { Ok(d) } else { Err(MError::expr("The date is out of range.")) }
}

// ---------------------------------------------------------------- text helpers

fn long_text(s: String) -> MResult<MValue> {
    if s.len() > super::super::MAX_TEXT {
        return Err(limit("The text is too long."));
    }
    Ok(MValue::text(s))
}

fn map_text(v: &MValue, f: impl Fn(&str) -> String) -> MResult<MValue> {
    match v {
        MValue::Null => Ok(MValue::Null),
        MValue::Text(t) => Ok(MValue::text(f(t))),
        other => Err(cant(other, "Text")),
    }
}

fn proper(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut start = true;
    for c in s.chars() {
        if c.is_alphabetic() {
            if start {
                out.extend(c.to_uppercase());
            } else {
                out.extend(c.to_lowercase());
            }
            start = false;
        } else {
            out.push(c);
            start = !c.is_alphanumeric() && c != '\'';
        }
    }
    out
}

/// `Replacer.ReplaceText`: text inside text values; other values unchanged.
pub fn replace_text(v: &MValue, old: &MValue, new: &MValue) -> MResult<MValue> {
    let MValue::Text(t) = v else { return Ok(v.clone()) };
    let MValue::Text(o) = old else { return Err(cant(old, "Text")) };
    let n = match new {
        MValue::Null => Rc::from(""),
        MValue::Text(n) => n.clone(),
        other => return Err(cant(other, "Text")),
    };
    if o.is_empty() {
        return Ok(v.clone());
    }
    let count = t.matches(&**o).count();
    if t.len().saturating_add(count.saturating_mul(n.len())) > super::super::MAX_TEXT {
        return Err(limit("The text is too long."));
    }
    Ok(MValue::text(t.replace(&**o, &n)))
}

fn is_ignore_case(v: &MValue) -> bool {
    matches!(v, MValue::Function(f) if matches!(&**f, super::super::value::MFunc::Builtin { name, .. } if &**name == "Comparer.OrdinalIgnoreCase"))
}

fn folded(v: &MValue, fold: bool) -> MValue {
    match v {
        MValue::Text(t) if fold => MValue::text(t.to_lowercase()),
        v => v.clone(),
    }
}

fn folded_key(v: &MValue, fold: bool) -> Key {
    key(&folded(v, fold))
}

/// A list's items worked out (a held error is raised).
fn forced(ev: &mut Ev<'_, '_>, l: &[MValue]) -> MResult<Vec<MValue>> {
    let mut out = Vec::with_capacity(l.len());
    for v in l {
        out.push(ev.force(v.clone())?);
    }
    Ok(out)
}

/// A delimiter index argument: a number, or `{n, RelativePosition.FromEnd}`.
fn delimiter_index(v: &MValue) -> MResult<(usize, bool)> {
    let n = |x: &MValue| match x {
        MValue::Number(n) if *n >= 0.0 && n.fract() == 0.0 => Ok(*n as usize),
        other => Err(cant(other, "Number")),
    };
    match v {
        MValue::Null => Ok((0, false)),
        MValue::Number(_) => Ok((n(v)?, false)),
        MValue::List(l) => {
            let i = l.first().map(n).transpose()?.unwrap_or(0);
            let from_end = matches!(l.get(1), Some(MValue::Number(x)) if *x == 1.0);
            Ok((i, from_end))
        }
        other => Err(cant(other, "Number")),
    }
}

/// The byte offset of the `n`th (from 0) occurrence of `d` in `t`, from the start or the end.
fn find_nth(t: &str, d: &str, n: usize, from_end: bool) -> Option<usize> {
    if d.is_empty() {
        return None;
    }
    if from_end { t.rmatch_indices(d).nth(n).map(|(i, _)| i) } else { t.match_indices(d).nth(n).map(|(i, _)| i) }
}

#[derive(Clone, Copy)]
enum Split {
    Delimiter,
    Each,
    Positions,
    Lengths,
}

/// A splitter applied to its input (the last argument; the splitter's settings come first).
fn split_apply(a: &Args<'_>, kind: Split) -> MResult<MValue> {
    let input = a.v.last().cloned().unwrap_or(MValue::Null);
    let cfg = Args { name: a.name, v: a.v.get(..a.v.len().saturating_sub(1)).unwrap_or(&[]).to_vec() };
    let text = match &input {
        MValue::Null => return Ok(MValue::list(vec![MValue::Null])),
        MValue::Text(t) => t.clone(),
        other => return Err(cant(other, "Text")),
    };
    let parts: Vec<String> = match kind {
        Split::Delimiter => {
            let d = cfg.text(0)?;
            if d.is_empty() {
                return Err(MError::expr("The delimiter can't be empty."));
            }
            let csv = matches!(cfg.at(1), MValue::Number(n) if *n == 1.0);
            if csv { split_quoted(&text, &d) } else { text.split(&*d).map(str::to_string).collect() }
        }
        Split::Each => {
            let delims = super::names_of(cfg.at(0))?;
            let from_end = matches!(cfg.at(2), MValue::Logical(true));
            let mut parts: Vec<String> = Vec::new();
            let mut rest = text.to_string();
            for d in delims.iter().filter(|d| !d.is_empty()) {
                let at = if from_end { rest.rfind(&**d) } else { rest.find(&**d) };
                let Some(p) = at else { break };
                let (head, tail) = (rest.get(..p).unwrap_or("").to_string(), rest.get(p + d.len()..).unwrap_or("").to_string());
                if from_end {
                    parts.insert(0, tail);
                    rest = head;
                } else {
                    parts.push(head);
                    rest = tail;
                }
            }
            if from_end {
                parts.insert(0, rest);
            } else {
                parts.push(rest);
            }
            parts
        }
        Split::Positions | Split::Lengths => {
            let l = cfg.list(0)?;
            let mut nums = Vec::with_capacity(l.len());
            for x in l.iter() {
                match x {
                    MValue::Number(n) if *n >= 0.0 && n.fract() == 0.0 => nums.push(*n as usize),
                    other => return Err(cant(other, "Number")),
                }
            }
            let u: Vec<u16> = text.encode_utf16().collect();
            let mut cuts: Vec<(usize, usize)> = Vec::new();
            match kind {
                Split::Positions => {
                    for (i, p) in nums.iter().enumerate() {
                        let end = nums.get(i + 1).copied().unwrap_or(u.len());
                        cuts.push(((*p).min(u.len()), end.min(u.len()).max((*p).min(u.len()))));
                    }
                }
                _ => {
                    let mut at = 0usize;
                    for n in &nums {
                        let end = at.saturating_add(*n).min(u.len());
                        cuts.push((at, end));
                        at = end;
                    }
                }
            }
            cuts.into_iter().map(|(s, e)| String::from_utf16_lossy(u.get(s..e).unwrap_or(&[]))).collect()
        }
    };
    if parts.len() > MAX_LIST {
        return Err(limit("The text splits into too many parts."));
    }
    Ok(MValue::list(parts.into_iter().map(MValue::text).collect()))
}

/// Splits CSV-style: a field starting with a quote runs to its closing quote (`""` is a quote).
fn split_quoted(text: &str, d: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut rest = text;
    let mut field_start = true;
    let mut quoted = false;
    while !rest.is_empty() {
        if quoted {
            if let Some(r) = rest.strip_prefix("\"\"") {
                cur.push('"');
                rest = r;
            } else if let Some(r) = rest.strip_prefix('"') {
                quoted = false;
                rest = r;
            } else {
                let mut cs = rest.chars();
                if let Some(c) = cs.next() {
                    cur.push(c);
                }
                rest = cs.as_str();
            }
            continue;
        }
        if field_start && let Some(r) = rest.strip_prefix('"') {
            quoted = true;
            field_start = false;
            rest = r;
            continue;
        }
        if let Some(r) = rest.strip_prefix(d) {
            out.push(std::mem::take(&mut cur));
            field_start = true;
            rest = r;
            continue;
        }
        let mut cs = rest.chars();
        if let Some(c) = cs.next() {
            cur.push(c);
        }
        rest = cs.as_str();
        field_start = false;
    }
    out.push(cur);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding_modes() {
        assert_eq!(round(2.5, 0, 4), 2.0);
        assert_eq!(round(3.5, 0, 4), 4.0);
        assert_eq!(round(-2.5, 0, 4), -2.0);
        assert_eq!(round(2.5, 0, 2), 3.0);
        assert_eq!(round(1.234, 2, 0), 1.24);
        assert_eq!(round(1.239, 2, 1), 1.23);
        assert_eq!(round(1234.0, -2, 4), 1200.0);
    }

    #[test]
    fn conversions_follow_m() {
        let n = MType::prim("number");
        assert!(matches!(convert(&MValue::text("1,5"), &n, Some("de-DE"), DateSystem::D1900), MValue::Number(x) if x == 1.5));
        assert!(matches!(convert(&MValue::text("abc"), &n, None, DateSystem::D1900), MValue::Error(_)));
        let int = MType::facet("number", "Int64.Type");
        assert!(matches!(convert(&MValue::Number(2.5), &int, None, DateSystem::D1900), MValue::Number(x) if x == 2.0));
        let d = convert(&MValue::text("1/31/2026"), &MType::prim("date"), None, DateSystem::D1900);
        assert!(matches!(d, MValue::Date(x) if x == days(2026, 1, 31).unwrap()), "{d:?}");
        assert!(
            matches!(convert(&MValue::Number(46_053.0), &MType::prim("date"), None, DateSystem::D1900), MValue::Date(x) if x == days(2026, 1, 31).unwrap())
        );
        assert_eq!(text_of(&MValue::Date(days(2026, 1, 31).unwrap()), None).unwrap(), "1/31/2026");
        assert_eq!(text_of(&MValue::Number(1.5), Some("de-DE")).unwrap(), "1,5");
        assert!(
            matches!(convert(&MValue::text("1.02:30:00"), &MType::prim("duration"), None, DateSystem::D1900), MValue::Duration(x) if (x - (1.0 + 2.5 / 24.0)).abs() < 1e-9)
        );
    }

    #[test]
    fn text_helpers() {
        assert_eq!(proper("hello WORLD o'neil"), "Hello World O'neil");
        assert_eq!(split_quoted("a,\"b,c\",\"d\"\"e\"", ","), vec!["a", "b,c", "d\"e"]);
        assert_eq!(find_nth("a-b-c", "-", 1, false), Some(3));
        assert_eq!(find_nth("a-b-c", "-", 0, true), Some(3));
    }
}
