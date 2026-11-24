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

/// A price: two decimals, four under a dollar, six under a cent, none for coins in the tens of thousands.
#[must_use]
pub fn price(value: f64, kind: Kind) -> String {
    let abs = value.abs();
    if kind == Kind::Indicator {
        return indicator(value);
    }
    let decimals = if abs > 0.0 && abs < 0.01 {
        6
    } else if abs < 1.0 {
        4
    } else if matches!(kind, Kind::Crypto | Kind::Perp) && abs >= 10_000.0 {
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

/// The fewest decimals, up to `max`, that show `value` rounded to `max` places.
fn needed_decimals(value: f64, max: u32) -> u32 {
    let scale = |d: u32| 10_f64.powi(i32::try_from(d).unwrap_or(0));
    let target = (value * scale(max)).round() / scale(max);
    (0..=max).find(|d| ((target * scale(*d)) - (target * scale(*d)).round()).abs() < 1e-7).unwrap_or(max)
}

/// A value of an economic indicator: up to three decimals under ten, two under a thousand, none above,
/// without trailing zeros ("55.5", "4.1", "337.51", "165,000"); a huge one is cut short ("-105B").
#[must_use]
pub fn indicator(value: f64) -> String {
    let abs = value.abs();
    let mut out = if abs >= 1e7 {
        compact(abs)
    } else {
        let max = if abs < 10.0 { 3 } else if abs < 1_000.0 { 2 } else { 0 };
        magnitude(abs, needed_decimals(abs, max))
    };
    if value < 0.0 && abs >= 0.0005 {
        out.insert(0, '-');
    }
    out
}

/// The change of an indicator in its own unit, with its sign: "+1", "-0.1", "+105,000", "0".
#[must_use]
pub fn delta(value: f64) -> String {
    let body = indicator(value.abs());
    if value.abs() < 0.0005 {
        return body;
    }
    let mut out = String::from(if value < 0.0 { '-' } else { '+' });
    out.push_str(&body);
    out
}

/// An amount in a few characters: "950", "5.4K", "482M", "1.6B", "2.3T".
#[must_use]
pub fn compact(value: f64) -> String {
    let abs = value.abs();
    let (scaled, suffix) = if abs >= 1e12 {
        (abs / 1e12, "T")
    } else if abs >= 1e9 {
        (abs / 1e9, "B")
    } else if abs >= 1e6 {
        (abs / 1e6, "M")
    } else if abs >= 1e3 {
        (abs / 1e3, "K")
    } else {
        (abs, "")
    };
    let decimals = u32::from(!suffix.is_empty() && scaled < 10.0);
    let mut out = magnitude(scaled, decimals);
    out.push_str(suffix);
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
    fn perpetuals_follow_the_same_rules_and_tiny_prices_keep_their_digits() {
        assert_eq!(price(163.43, Kind::Perp), "163.43");
        assert_eq!(price(60_317.0, Kind::Perp), "60,317");
        assert_eq!(price(4_512.3, Kind::Perp), "4,512.30");
        assert_eq!(price(0.0038, Kind::Perp), "0.003800");
        assert_eq!(price(0.0, Kind::Perp), "0.0000");
    }

    #[test]
    fn indicator_values_keep_only_the_decimals_they_need() {
        assert_eq!(indicator(55.5), "55.5");
        assert_eq!(indicator(4.1), "4.1");
        assert_eq!(indicator(50.0), "50");
        assert_eq!(indicator(3.7), "3.7");
        assert_eq!(indicator(2.07), "2.07");
        assert_eq!(indicator(337.511), "337.51");
        assert_eq!(indicator(165_000.0), "165,000");
        assert_eq!(indicator(-0.4), "-0.4");
        assert_eq!(indicator(-105_047_000_000.0), "-105B");
        assert_eq!(indicator(2_300_000_000_000.0), "2.3T");
        assert_eq!(price(55.5, Kind::Indicator), "55.5");
    }

    #[test]
    fn indicator_changes_carry_their_sign_and_no_float_noise() {
        assert_eq!(delta(1.000_000_000_000_028_4), "+1");
        assert_eq!(delta(-0.100_000_000_000_000_53), "-0.1");
        assert_eq!(delta(0.571_000_000_000_026_4), "+0.571");
        assert_eq!(delta(105_000.0), "+105,000");
        assert_eq!(delta(0.0), "0");
        assert_eq!(delta(-0.000_000_1), "0");
    }

    #[test]
    fn amounts_are_cut_to_a_few_characters() {
        assert_eq!(compact(950.0), "950");
        assert_eq!(compact(5_400.0), "5.4K");
        assert_eq!(compact(12_500.0), "13K");
        assert_eq!(compact(482_300_000.0), "482M");
        assert_eq!(compact(1_615_050_662.0), "1.6B");
        assert_eq!(compact(23_000_000_000.0), "23B");
        assert_eq!(compact(2_300_000_000_000.0), "2.3T");
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
