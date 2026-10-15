//! `List.*`.

use super::super::eval::{Ev, MAX_LIST, limit};
use super::super::value::{MError, MResult, MValue, equal, order, sort_order};
use super::{Args, cant, folded, folded_key, forced, is_ignore_case, show};

pub fn call(ev: &mut Ev<'_, '_>, name: &str, a: &Args<'_>) -> MResult<Option<MValue>> {
    let r = match name {
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
        _ => return Ok(None),
    };
    Ok(Some(r))
}
