//! Totals for the selected cells, shown in the status bar: how many have a
//! value, and when they're all numbers, their sum, average, smallest and
//! largest. Sums are exact (decimal arithmetic), so money adds up to the cent.

use std::cmp::Ordering;

use xenon_driver::Value;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Stats {
    /// Cells with a value (not NULL).
    pub count: usize,
    /// Present when every value is a number.
    pub numbers: Option<Numbers>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Numbers {
    pub sum: String,
    pub average: String,
    pub min: String,
    pub max: String,
}

/// A number held exactly: `mantissa × 10^-scale`.
#[derive(Clone, Copy, Debug)]
struct Exact {
    mantissa: i128,
    scale: u32,
}

impl Exact {
    fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let (negative, digits) = match text.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, text.strip_prefix('+').unwrap_or(text)),
        };
        let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
        let plain = |s: &str| s.chars().all(|c| c.is_ascii_digit());
        if whole.is_empty() && fraction.is_empty()
            || !plain(whole)
            || !plain(fraction)
            || whole.len() + fraction.len() > 30
        {
            return None;
        }
        let mantissa: i128 = format!("{whole}{fraction}").parse().ok()?;
        Some(Self {
            mantissa: if negative { -mantissa } else { mantissa },
            scale: fraction.len() as u32,
        })
    }

    fn of(value: &Value) -> Option<Self> {
        match value {
            Value::Integer(n) => Some(Self {
                mantissa: i128::from(*n),
                scale: 0,
            }),
            Value::Real(n) if n.is_finite() => Self::parse(&format!("{n}")),
            Value::Decimal(text) | Value::Text(text) => Self::parse(text),
            _ => None,
        }
    }

    fn rescale(self, scale: u32) -> Option<i128> {
        self.mantissa
            .checked_mul(10i128.checked_pow(scale - self.scale)?)
    }

    fn to_f64(self) -> f64 {
        self.mantissa as f64 / 10f64.powi(self.scale as i32)
    }

    fn display(mantissa: i128, scale: u32) -> String {
        let negative = mantissa < 0;
        let digits = mantissa.unsigned_abs().to_string();
        let digits = format!("{digits:0>width$}", width = scale as usize + 1);
        let (whole, fraction) = digits.split_at(digits.len() - scale as usize);
        let whole = group(whole);
        let sign = if negative { "-" } else { "" };
        if fraction.is_empty() {
            format!("{sign}{whole}")
        } else {
            format!("{sign}{whole}.{fraction}")
        }
    }
}

fn group(digits: &str) -> String {
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Totals for `values` (the selected cells), or `None` for fewer than two.
pub(crate) fn of<'a>(values: impl IntoIterator<Item = &'a Value>) -> Option<Stats> {
    let mut cells = 0;
    let mut present = Vec::new();
    for value in values {
        cells += 1;
        if !value.is_null() {
            present.push(value);
        }
    }
    if cells < 2 {
        return None;
    }
    let count = present.len();
    let exact: Option<Vec<Exact>> = present.iter().map(|v| Exact::of(v)).collect();
    let numbers = exact.filter(|e| !e.is_empty()).and_then(|exact| {
        let scale = exact.iter().map(|e| e.scale).max().unwrap_or(0);
        let mut sum: i128 = 0;
        for e in &exact {
            sum = sum.checked_add(e.rescale(scale)?)?;
        }
        let cmp = |a: &&Exact, b: &&Exact| {
            a.to_f64()
                .partial_cmp(&b.to_f64())
                .unwrap_or(Ordering::Equal)
        };
        let min = exact.iter().min_by(cmp)?;
        let max = exact.iter().max_by(cmp)?;
        let average = sum as f64 / 10f64.powi(scale as i32) / exact.len() as f64;
        Some(Numbers {
            sum: Exact::display(sum, scale),
            average: format_average(average, scale),
            min: Exact::display(min.mantissa, min.scale),
            max: Exact::display(max.mantissa, max.scale),
        })
    });
    Some(Stats { count, numbers })
}

/// An average to two more places than the values had (at most six).
fn format_average(average: f64, scale: u32) -> String {
    let places = (scale + 2).min(6) as usize;
    let text = format!("{average:.places$}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    let (sign, rest) = match text.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", text),
    };
    let (whole, fraction) = rest.split_once('.').unwrap_or((rest, ""));
    if fraction.is_empty() {
        format!("{sign}{}", group(whole))
    } else {
        format!("{sign}{}.{fraction}", group(whole))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_add_up_exactly() {
        let values = [
            Value::Decimal("0.10".into()),
            Value::Decimal("0.20".into()),
            Value::Integer(1000),
            Value::Null,
            Value::Text("1234.5".into()),
        ];
        let stats = of(&values).unwrap();
        assert_eq!(stats.count, 4);
        let numbers = stats.numbers.unwrap();
        assert_eq!(numbers.sum, "2,234.80");
        assert_eq!(numbers.min, "0.10");
        assert_eq!(numbers.max, "1,234.5");
        assert_eq!(numbers.average, "558.7");
    }

    #[test]
    fn text_gets_a_count_only() {
        let values = [Value::Text("paid".into()), Value::Integer(2), Value::Null];
        assert_eq!(
            of(&values),
            Some(Stats {
                count: 2,
                numbers: None
            })
        );
        // One cell isn't a selection worth totalling.
        assert_eq!(of(&[Value::Integer(1)]), None);
    }

    #[test]
    fn negatives_and_reals() {
        let values = [Value::Integer(-5), Value::Real(2.5), Value::Integer(-1)];
        let numbers = of(&values).unwrap().numbers.unwrap();
        assert_eq!(numbers.sum, "-3.5");
        assert_eq!(numbers.min, "-5");
        assert_eq!(numbers.max, "2.5");
        assert_eq!(numbers.average, "-1.167");
    }
}
