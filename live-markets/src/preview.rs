//! Sample data for looking at the screens on a Mac, in the SDK's desktop testbed, in sessions and
//! time frames that the live markets are not in right now. It is built in only with the `preview`
//! feature, and the sample is picked when the widget is built:
//!
//! ```text
//! LM_PREVIEW=post cargo build -p live-markets --features preview --target wasm32-unknown-unknown
//! ```
//!
//! The values are `pre`, `post`, `closed`, `offline` for the list, and `chart:1D`, `chart:5D`,
//! `chart:1M`, `chart:6M`, `chart:1Y`, `chart:5Y` (optionally `:post` or `:pre` after it, and then
//! `:50` for a finger half way along the chart) for the chart of the first symbol. The lists above
//! are CNBC's; `hl` is the list as it comes from Hyperliquid, with JPM and SPY still from CNBC, and
//! `chart:1D:hl` is the chart of its first symbol. `livechart:1D` is the real markets with that chart
//! open, for seeing how a live one loads.

use crate::chart::{Frame, RawBar, Series};
use crate::hl::Ctx;
use crate::quote::Raw;
use crate::state::{Detail, Screen, Scrub, State};
use crate::symbols::{Kind, Source};

const MODE: Option<&str> = option_env!("LM_PREVIEW");

/// symbol, name, previous close, regular last, pre-market price, after-hours price
const SAMPLES: [(&str, &str, f64, f64, f64, f64); 8] = [
    ("NVDA", "NVIDIA Corporation", 125.53, 165.60, 167.06, 166.20),
    ("TSLA", "Tesla Inc", 345.82, 391.67, 374.74, 393.10),
    ("MSTR", "Strategy", 77.80, 87.74, 90.67, 88.90),
    ("META", "Meta Platforms, Inc.", 560.84, 593.26, 591.29, 594.80),
    ("NFLX", "Netflix, Inc.", 56.73, 57.96, 58.58, 57.40),
    ("JPM", "JPMorgan Chase & Co", 310.53, 319.88, 319.98, 320.50),
    ("SPY", "SPDR S&P 500 ETF Trust", 689.81, 724.13, 724.42, 723.20),
    ("BTC.CM=", "Bitcoin/USD Coin Metrics", 60_649.19, 62_590.66, 0.0, 0.0),
];

/// coin, mark price, price a day ago, volume of the day: what Hyperliquid says of the markets that
/// the first symbols of the default list have there.
const PERPS: [(&str, &str, &str, &str); 6] = [
    ("xyz:NVDA", "163.43", "134.51", "1615050662.07"),
    ("xyz:TSLA", "389.40", "381.10", "980144210.50"),
    ("xyz:MSTR", "88.10", "77.80", "240551932.10"),
    ("xyz:META", "594.20", "560.84", "310887004.90"),
    ("xyz:NFLX", "57.80", "56.73", "88120400.00"),
    ("BTC", "60317.0", "61521.0", "4817910048.40"),
];

/// A price as the service writes it.
fn text(value: f64) -> String {
    format!("{value:.2}")
}

fn percent(value: f64, base: f64) -> String {
    let pct = (value / base - 1.0) * 100.0;
    format!("{pct:+.2}%")
}

fn replies(mode: &str) -> Vec<Raw> {
    SAMPLES
        .iter()
        .map(|&(symbol, name, prev, last, pre, post)| {
            let coin = symbol.ends_with("CM=");
            let (status, shown_last, ext_type, ext_last, ext_pct) = match mode {
                _ if coin => ("REG_MKT", last, "", 0.0, String::new()),
                "pre" => ("PRE_MKT", prev, "PRE_MKT", pre, percent(pre, prev)),
                "post" => ("POST_MKT", last, "POST_MKT", post, percent(post, last)),
                "closed" => ("CLOSED", last, "POST_MKT", post, percent(post, last)),
                _ => ("REG_MKT", last, "PRE_MKT", pre, percent(pre, prev)),
            };
            Raw {
                symbol: symbol.to_string(),
                code: 0,
                name: name.to_string(),
                last: text(shown_last),
                prev_close: text(prev),
                change_pct: percent(shown_last, prev),
                status: status.to_string(),
                real_time: "true".to_string(),
                last_time: "2026-11-23T11:03:39.723-0500".to_string(),
                ext_type: ext_type.to_string(),
                ext_last: if ext_type.is_empty() { String::new() } else { text(ext_last) },
                ext_change_pct: ext_pct,
                ext_time: "2026-11-23T09:29:59.980-0500".to_string(),
            }
        })
        .collect()
}

/// A made-up walk of prices that ends near `end`, from a fixed seed so that shots repeat.
fn walk(count: usize, end: f64, wobble: f64) -> Vec<f64> {
    let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
    let mut out = Vec::with_capacity(count);
    let mut value = 0.0;
    for _ in 0..count {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        #[expect(clippy::cast_precision_loss, reason = "sample data")]
        let step = ((seed >> 33) as f64 / f64::from(1_u32 << 31) - 0.5) * 2.0;
        value += step * wobble + wobble * 0.05;
        out.push(value);
    }
    let last = out.last().copied().unwrap_or(0.0);
    out.iter().map(|v| end + (v - last)).collect()
}

/// The bars of a frame that end near `end`, and begin near `start` when it is given.
fn series(frame: Frame, kind: Kind, end: f64, start: Option<f64>) -> Series {
    const DAY: i64 = 86_400;
    let today = 20_780 * DAY;
    // (bars, seconds between them, first bar's second of the day, bars per day)
    let (count, step, from, per_day): (usize, i64, i64, usize) = match frame {
        // A perpetual trades all day and night: a full day of five minutes, five days of half hours.
        Frame::Day if kind == Kind::Perp => (288, 300, 8 * 3_600 + 40 * 60, 288),
        Frame::Week if kind == Kind::Perp => (5 * 48, 1_800, 0, 48),
        Frame::Day => (90, 600, 4 * 3_600, 90),
        Frame::Week => (5 * 64, 900, 4 * 3_600, 64),
        Frame::Month => (22, DAY, 0, 1),
        Frame::HalfYear => (126, DAY, 0, 1),
        Frame::Year => (250, DAY, 0, 1),
        Frame::FiveYears => (260, 7 * DAY, 0, 1),
    };
    let mut closes = walk(count, end, end * 0.004 * if frame.intraday() { 1.0 } else { 3.0 });
    if let (Some(start), Some(first)) = (start, closes.first().copied()) {
        // Bend the walk so that it begins at `start` and still ends where it did.
        #[expect(clippy::cast_precision_loss, reason = "sample data")]
        for (i, close) in closes.iter_mut().enumerate() {
            *close += (start - first) * (1.0 - i as f64 / (count - 1) as f64);
        }
    }
    let bars: Vec<RawBar> = closes
        .iter()
        .enumerate()
        .map(|(i, close)| {
            let day = i64::try_from(i / per_day).unwrap_or(0);
            let within = i64::try_from(i % per_day).unwrap_or(0);
            let last_day = i64::try_from((count - 1) / per_day).unwrap_or(0);
            let local = if frame.intraday() {
                today - (last_day - day) * DAY + from + within * step
            } else {
                today - i64::try_from(count - 1 - i).unwrap_or(0) * step
            };
            RawBar { ms: (local + 5 * 3_600) * 1_000, local, close: *close }
        })
        .collect();
    Series::from_bars(frame, kind, &bars)
}

/// The list as Hyperliquid and CNBC share it: what the first has a market for comes from it.
fn sample_hl(state: &mut State, now: i64) {
    for row in &mut state.rows {
        if !matches!(row.entry.query.as_str(), "JPM" | "SPY") {
            row.source = Source::Hyperliquid;
        }
    }
    state.apply(&replies("regular"), now);
    for (coin, mark, prev, volume) in PERPS {
        let ctx = Ctx { mark: mark.into(), mid: String::new(), prev_day: prev.into(), volume: volume.into() };
        state.apply_hl(coin, &ctx, now);
    }
}

fn frame_of(name: &str) -> Frame {
    match name {
        "5D" => Frame::Week,
        "1M" => Frame::Month,
        "6M" => Frame::HalfYear,
        "1Y" => Frame::Year,
        "5Y" => Frame::FiveYears,
        _ => Frame::Day,
    }
}

/// Fills the state from the mode picked at build time. Whether it did, which turns the live fetching off.
pub fn seed(state: &mut State, now: i64) -> bool {
    let Some(mode) = MODE else { return false };
    // `livechart:1D`: the real markets with the chart of the first symbol open, to see how a live one loads.
    if let Some(frame) = mode.strip_prefix("livechart:") {
        state.screen = Screen::Detail(Detail::new(0, frame_of(frame)));
        state.touched_ms = u64::MAX / 2;
        return false;
    }
    let list = mode.split(':').next().unwrap_or("");
    let session = if list == "chart" { mode.split(':').nth(2).unwrap_or("regular") } else { list };
    // The samples are CNBC's, but for the one that shows both sources.
    for row in &mut state.rows {
        row.source = Source::Cnbc;
    }
    if session == "hl" {
        sample_hl(state, now);
    } else {
        state.apply(&replies(session), now);
    }
    // Never late, so that the sample does not dim after a minute.
    state.last_ok = if list == "offline" { now - 600 } else { 5_000_000_000 };
    if list == "chart" {
        let frame = frame_of(mode.split(':').nth(1).unwrap_or("1D"));
        let kind = state.rows[0].kind();
        // A perpetual's day begins at the price a day ago and ends at its price now.
        let (end, start) = if kind == Kind::Perp {
            (PERPS[0].1.parse().unwrap_or(SAMPLES[0].3), PERPS[0].2.parse().ok())
        } else {
            (SAMPLES[0].3, None)
        };
        let mut detail = Detail::new(0, frame);
        detail.series = Some(series(frame, kind, end, start.filter(|_| frame == Frame::Day)));
        detail.scrub = mode.split(':').nth(3).and_then(|p| p.parse::<f32>().ok()).map(|p| Scrub::Fraction(p / 100.0));
        state.screen = Screen::Detail(detail);
        state.touched_ms = u64::MAX / 2;
    }
    true
}
