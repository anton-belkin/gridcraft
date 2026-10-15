//! `Number.*`.

use super::super::eval::Ev;
use super::super::value::{MError, MResult, MValue};
use super::{Args, cant};

pub fn call(_ev: &mut Ev<'_, '_>, name: &str, a: &Args<'_>) -> MResult<Option<MValue>> {
    let r = match name {
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
        _ => return Ok(None),
    };
    Ok(Some(r))
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

pub fn num1(v: &MValue, f: impl Fn(f64) -> f64) -> MResult<MValue> {
    match v {
        MValue::Null => Ok(MValue::Null),
        MValue::Number(n) => Ok(MValue::Number(f(*n))),
        other => Err(cant(other, "Number")),
    }
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
}
