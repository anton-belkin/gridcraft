//! Links inside text (Topics v1, §2 and §2a). Answers, notes, status text and questions keep their
//! links to cells as anchors, so they follow the cells: insert a row and a past answer that said
//! `C4` says `C5`, with nothing marking the change.
//!
//! Stored form: Markdown links whose target is `anchor:<id>`; the label `@` means "the current
//! address". [`store`] turns `[B1](cell:Sheet1!B1)` and plain, unambiguous references (`C3:D9`,
//! `Sheet2!B4`, or a single `B1` that holds something) into that form; [`segments`] and [`render`]
//! turn it back into current addresses.

use gridcraft_engine::Session;
use gridcraft_engine::model::Workbook;

use crate::anchor::{self, Place};
use crate::layer::Layer;

/// A piece of a text as shown.
#[derive(Clone, Debug, PartialEq)]
pub enum Seg {
    Text(String),
    /// A link: what it says and where it leads (`cell:Sheet1!B2`, `topic:3`, `https://…`).
    /// `removed`: the cells it pointed at were deleted.
    Link {
        label: String,
        href: String,
        removed: bool,
    },
}

/// `B2` on `home`, `Sheet2!B2` elsewhere.
pub fn address(p: &Place, home: usize) -> String {
    if p.sheet == home { p.range.a1() } else { p.a1() }
}

fn is_col(c: char) -> bool {
    c.is_ascii_uppercase()
}

/// A cell reference at the start of `s` (`$A$1`, `B12`): its length.
fn cell_len(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = 0;
    if b.get(i) == Some(&b'$') {
        i += 1;
    }
    let letters = b.get(i..).map_or(0, |r| r.iter().take_while(|c| is_col(**c as char)).count());
    if !(1..=3).contains(&letters) {
        return None;
    }
    i += letters;
    if b.get(i) == Some(&b'$') {
        i += 1;
    }
    let digits = b.get(i..).map_or(0, |r| r.iter().take_while(|c| c.is_ascii_digit()).count());
    if !(1..=7).contains(&digits) || b.get(i) == Some(&b'0') {
        return None;
    }
    Some(i + digits)
}

/// A plain reference at the start of `s`: `[sheet!]A1[:B2]` → (length, text).
fn plain_ref(s: &str) -> Option<(usize, bool)> {
    let mut i = 0;
    let mut qualified = false;
    // A sheet: 'Quoted name'! or Name!
    if s.starts_with('\'') {
        let end = s.get(1..)?.find("'!")? + 1;
        i = end + 2;
        qualified = true;
    } else if let Some(bang) = s.find('!') {
        let name = s.get(..bang)?;
        if !name.is_empty()
            && name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.')
            && name.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_')
        {
            i = bang + 1;
            qualified = true;
        }
    }
    let first = cell_len(s.get(i..)?)?;
    i += first;
    let mut range = false;
    if s.get(i..)?.starts_with(':')
        && let Some(n) = s.get(i + 1..).and_then(cell_len)
    {
        i += 1 + n;
        range = true;
    }
    Some((i, qualified || range))
}

/// The label a cell link stores: `@` when it says the address (so it follows the cells).
fn stored_label(label: &str, p: &Place) -> String {
    let l = label.trim().replace('$', "");
    if l == p.range.a1() || l == p.a1() || l.eq_ignore_ascii_case(&p.a1()) { "@".into() } else { label.to_string() }
}

/// The text as stored: links to cells and unambiguous references become anchors.
pub fn store(s: &mut Session, text: &str) -> gridcraft_engine::Result<String> {
    let mut out = String::with_capacity(text.len() + 16);
    let mut rest = text;
    let mut prev: Option<char> = None;
    let mut budget = 200usize;
    while let Some(c) = rest.chars().next() {
        // A Markdown link.
        if c == '['
            && let Some(close) = rest.find("](")
            && let Some(end) = rest.get(close + 2..).and_then(|r| r.find(')'))
        {
            let label = rest.get(1..close).unwrap_or("");
            let href = rest.get(close + 2..close + 2 + end).unwrap_or("");
            if !label.contains('\n') && !href.contains(char::is_whitespace) {
                let target = href.strip_prefix("cell:");
                let place = match target {
                    Some(t) if budget > 0 => {
                        let d = s.doc()?;
                        anchor::parse_place(&d.wb, t, d.wb.active_sheet)
                    }
                    _ => None,
                };
                match place {
                    Some(p) => {
                        budget -= 1;
                        let a = anchor::anchor_at(s, &p)?;
                        out.push_str(&format!("[{}](anchor:{a})", stored_label(label, &p)));
                    }
                    None => out.push_str(rest.get(..close + 3 + end).unwrap_or("")),
                }
                rest = rest.get(close + 3 + end..).unwrap_or("");
                prev = Some(')');
                continue;
            }
        }
        // A web address: copied as it is.
        if (rest.starts_with("http://") || rest.starts_with("https://")) && prev.is_none_or(|p| !p.is_alphanumeric()) {
            let n = rest.find(char::is_whitespace).unwrap_or(rest.len());
            out.push_str(rest.get(..n).unwrap_or(""));
            rest = rest.get(n..).unwrap_or("");
            prev = Some('/');
            continue;
        }
        // A plain reference, at a word boundary.
        let starts = prev.is_none_or(|p| !(p.is_alphanumeric() || matches!(p, '_' | '.' | '$' | '\'' | ':' | '!')));
        if starts
            && budget > 0
            && (c == '\'' || c == '$' || c.is_alphabetic())
            && let Some((n, sure)) = plain_ref(rest)
            && rest.get(n..).and_then(|r| r.chars().next()).is_none_or(|x| !(x.is_alphanumeric() || matches!(x, '_' | '(' | '!' | ':')))
        {
            let token = rest.get(..n).unwrap_or("");
            let d = s.doc()?;
            let place = anchor::parse_place(&d.wb, token, d.wb.active_sheet);
            // A lone `B1` is a cell only when it holds something ("Q3 results" isn't a link).
            let holds =
                |p: &Place| d.wb.sheet(p.sheet).and_then(|sh| sh.cell(p.range.start)).is_some_and(|x| !x.value.is_empty() || x.formula.is_some());
            if let Some(p) = place.filter(|p| sure || holds(p)) {
                budget -= 1;
                let a = anchor::anchor_at(s, &p)?;
                out.push_str(&format!("[@](anchor:{a})"));
                rest = rest.get(n..).unwrap_or("");
                prev = Some('1');
                continue;
            }
        }
        out.push(c);
        rest = rest.get(c.len_utf8()..).unwrap_or("");
        prev = Some(c);
    }
    Ok(out)
}

/// What a deleted anchor reads as: "removed — was between B2 and B3" (live links).
pub fn removed(wb: &Workbook, l: &Layer, anchor_id: &str, home: usize) -> Vec<Seg> {
    let w = l.whereabouts.iter().rev().find(|w| w.anchor == anchor_id);
    let link = |id: &str| {
        l.anchor(id).and_then(|a| anchor::resolve(wb, a)).map(|p| Seg::Link {
            label: address(&p, home),
            href: format!("cell:{}", p.a1()),
            removed: false,
        })
    };
    let Some(w) = w else { return vec![Seg::Text("removed".into())] };
    let before = w.before.as_deref().and_then(link);
    let after = w.after.as_deref().and_then(link);
    // `C10` on its own sheet, `Sheet2!C10` elsewhere.
    let was = wb.sheet(home).map_or(w.was.as_str(), |sh| {
        let plain = format!("{}!", sh.name);
        let quoted = format!("'{}'!", sh.name.replace('\'', "''"));
        w.was.strip_prefix(&plain).or_else(|| w.was.strip_prefix(&quoted)).unwrap_or(&w.was)
    });
    let mut out = vec![Seg::Text(format!("removed (was {was}"))];
    match (before, after) {
        (Some(b), Some(a)) => {
            out.push(Seg::Text(", between ".into()));
            out.push(b);
            out.push(Seg::Text(" and ".into()));
            out.push(a);
        }
        (Some(b), None) => {
            out.push(Seg::Text(", after ".into()));
            out.push(b);
        }
        (None, Some(a)) => {
            out.push(Seg::Text(", before ".into()));
            out.push(a);
        }
        (None, None) => {}
    }
    out.push(Seg::Text(")".into()));
    out
}

/// A stored text as shown on `home`: anchors become current addresses.
pub fn segments(wb: &Workbook, l: &Layer, text: &str, home: usize) -> Vec<Seg> {
    let mut out: Vec<Seg> = vec![];
    let mut buf = String::new();
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if c == '['
            && let Some(close) = rest.find("](")
            && let Some(end) = rest.get(close + 2..).and_then(|r| r.find(')'))
        {
            let label = rest.get(1..close).unwrap_or("");
            let href = rest.get(close + 2..close + 2 + end).unwrap_or("");
            if !label.contains('\n') && !href.contains(char::is_whitespace) {
                if !buf.is_empty() {
                    out.push(Seg::Text(std::mem::take(&mut buf)));
                }
                match href.strip_prefix("anchor:") {
                    Some(id) => match l.anchor(id).and_then(|a| anchor::resolve(wb, a)) {
                        Some(p) => out.push(Seg::Link {
                            label: if label == "@" { address(&p, home) } else { label.to_string() },
                            href: format!("cell:{}", p.a1()),
                            removed: false,
                        }),
                        None => {
                            if label != "@" {
                                out.push(Seg::Text(format!("{label} ")));
                            }
                            out.extend(removed(wb, l, id, home));
                        }
                    },
                    None => out.push(Seg::Link { label: label.to_string(), href: href.to_string(), removed: false }),
                }
                rest = rest.get(close + 3 + end..).unwrap_or("");
                continue;
            }
        }
        buf.push(c);
        rest = rest.get(c.len_utf8()..).unwrap_or("");
    }
    if !buf.is_empty() {
        out.push(Seg::Text(buf));
    }
    out
}

/// A stored text for the agent: Markdown with current addresses (`[B2](cell:Sheet1!B2)`).
pub fn render(wb: &Workbook, l: &Layer, text: &str, home: usize) -> String {
    segments(wb, l, text, home)
        .into_iter()
        .map(|s| match s {
            Seg::Text(t) => t,
            Seg::Link { label, href, .. } => format!("[{label}]({href})"),
        })
        .collect()
}

/// The plain text (labels without targets), for short previews.
pub fn plain(wb: &Workbook, l: &Layer, text: &str, home: usize) -> String {
    segments(wb, l, text, home)
        .into_iter()
        .map(|s| match s {
            Seg::Text(t) => t,
            Seg::Link { label, .. } => label,
        })
        .collect()
}
