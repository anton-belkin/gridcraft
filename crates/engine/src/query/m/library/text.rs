//! `Text.*`. Positions count UTF-16 code units, as M does.

use std::rc::Rc;

use super::super::eval::{Ev, limit};
use super::super::value::{MResult, MValue};
use super::{Args, cant, is_ignore_case, long_text};

pub fn call(ev: &mut Ev<'_, '_>, name: &str, a: &Args<'_>) -> MResult<Option<MValue>> {
    let r = match name {
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
        _ => return Ok(None),
    };
    Ok(Some(r))
}

pub fn map_text(v: &MValue, f: impl Fn(&str) -> String) -> MResult<MValue> {
    match v {
        MValue::Null => Ok(MValue::Null),
        MValue::Text(t) => Ok(MValue::text(f(t))),
        other => Err(cant(other, "Text")),
    }
}

pub fn proper(s: &str) -> String {
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

/// A delimiter index argument: a number, or `{n, RelativePosition.FromEnd}`.
pub fn delimiter_index(v: &MValue) -> MResult<(usize, bool)> {
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
pub fn find_nth(t: &str, d: &str, n: usize, from_end: bool) -> Option<usize> {
    if d.is_empty() {
        return None;
    }
    if from_end { t.rmatch_indices(d).nth(n).map(|(i, _)| i) } else { t.match_indices(d).nth(n).map(|(i, _)| i) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_helpers() {
        assert_eq!(proper("hello WORLD o'neil"), "Hello World O'neil");
        assert_eq!(find_nth("a-b-c", "-", 1, false), Some(3));
        assert_eq!(find_nth("a-b-c", "-", 0, true), Some(3));
    }
}
