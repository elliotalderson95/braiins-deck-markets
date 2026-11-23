//! Numbers as text. Digit grouping and the decimal mark come from the Deck's own setting on the
//! device (`format_number!`); the tests and the previews use the plain English form.

use crate::symbols::Kind;

#[cfg(target_arch = "wasm32")]
fn magnitude(value: f64, decimals: u32) -> String {
    bmc_wasm_sdk::format_number!(value, decimals)
}

#[cfg(not(target_arch = "wasm32"))]
fn magnitude(value: f64, decimals: u32) -> String {
    let fixed = fixed(value, decimals);
    let (int, frac) = fixed.split_once('.').map_or((fixed.as_str(), ""), |(i, f)| (i, f));
    let mut out = String::new();
    for (n, digit) in int.chars().enumerate() {
        if n > 0 && (int.len() - n) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    if !frac.is_empty() {
        out.push('.');
        out.push_str(frac);
    }
    out
}

/// `value` rounded to `decimals` places, without grouping.
#[cfg(not(target_arch = "wasm32"))]
fn fixed(value: f64, decimals: u32) -> String {
    let scale = 10_f64.powi(i32::try_from(decimals).unwrap_or(2));
    let rounded = (value.abs() * scale).round();
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a price in cents")]
    let units = rounded as u64;
    let whole = units / 10_u64.pow(decimals);
    let part = units % 10_u64.pow(decimals);
    if decimals == 0 {
        whole.to_string()
    } else {
        format!("{whole}.{part:0width$}", width = decimals as usize)
    }
}

/// A price: two decimals, four under a dollar, none for coins in the tens of thousands.
#[must_use]
pub fn price(value: f64, kind: Kind) -> String {
    let abs = value.abs();
    let decimals = if abs < 1.0 {
        4
    } else if kind == Kind::Crypto && abs >= 10_000.0 {
        0
    } else {
        2
    };
    let mut out = magnitude(abs, decimals);
    if value < 0.0 {
        out.insert(0, '-');
    }
    out
}

/// A move in percent with its sign: "+1.23%", "-0.50%", "0.00%".
#[must_use]
pub fn percent(value: f64) -> String {
    let shown = (value * 100.0).round() / 100.0;
    let decimals = if shown.abs() >= 1000.0 { 0 } else { 2 };
    let mut out = String::new();
    if shown > 0.0 {
        out.push('+');
    } else if shown < 0.0 {
        out.push('-');
    }
    out.push_str(&magnitude(shown.abs(), decimals));
    out.push('%');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prices_use_two_decimals_and_group_thousands() {
        assert_eq!(price(348.75, Kind::Stock), "348.75");
        assert_eq!(price(7244.66, Kind::Index), "7,244.66");
        assert_eq!(price(24184.801, Kind::Index), "24,184.80");
        assert_eq!(price(5.0, Kind::Stock), "5.00");
    }

    #[test]
    fn coins_in_the_tens_of_thousands_drop_their_cents() {
        assert_eq!(price(62_804.86, Kind::Crypto), "62,805");
        assert_eq!(price(1_769.05, Kind::Crypto), "1,769.05");
    }

    #[test]
    fn prices_under_a_dollar_keep_four_decimals() {
        assert_eq!(price(0.4321, Kind::Stock), "0.4321");
    }

    #[test]
    fn moves_carry_their_sign() {
        assert_eq!(percent(1.234), "+1.23%");
        assert_eq!(percent(-0.5), "-0.50%");
        assert_eq!(percent(0.0), "0.00%");
        assert_eq!(percent(33.268), "+33.27%");
    }

    #[test]
    fn a_move_that_rounds_to_nothing_has_no_sign() {
        assert_eq!(percent(0.004), "0.00%");
        assert_eq!(percent(-0.004), "0.00%");
    }
}
