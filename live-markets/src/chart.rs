//! The chart behind a tap: the time frames, how CNBC's price bars become a series of points,
//! and where each point, band and label goes in a box of a given size.
//!
//! Everything here is plain arithmetic on numbers, so it is tested on the host. The glue reads
//! the bars out of the reply; `view` draws what `plot` works out.

use crate::quote::days_from_civil;
use crate::symbols::Kind;

/// The most points a chart is drawn from; the service sends up to thousands.
pub const MAX_POINTS: usize = 240;

const HOUR: i64 = 3_600;
const DAY: i64 = 86_400;
/// US stocks trade pre-market from 4:00, regularly from 9:30 to 16:00, after hours until 20:00.
const PRE_OPEN: i64 = 4 * HOUR;
const OPEN: i64 = 9 * HOUR + 30 * 60;
const CLOSE: i64 = 16 * HOUR;
const POST_CLOSE: i64 = 20 * HOUR;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Frame {
    Day,
    Week,
    Month,
    HalfYear,
    Year,
    FiveYears,
}

impl Frame {
    pub const ALL: [Self; 6] = [Self::Day, Self::Week, Self::Month, Self::HalfYear, Self::Year, Self::FiveYears];

    /// The button's text. Coins trade round the clock and the service keeps two days of them.
    #[must_use]
    pub fn label(self, kind: Kind) -> &'static str {
        match self {
            Self::Day => "1D",
            Self::Week if kind == Kind::Crypto => "2D",
            Self::Week => "5D",
            Self::Month => "1M",
            Self::HalfYear => "6M",
            Self::Year => "1Y",
            Self::FiveYears => "5Y",
        }
    }

    /// The service's name for the range that holds this frame.
    #[must_use]
    pub fn range(self) -> &'static str {
        match self {
            Self::Day => "1D",
            Self::Week => "5D",
            Self::Month | Self::HalfYear | Self::Year => "1M",
            Self::FiveYears => "5Y",
        }
    }

    /// How much of the range to keep, in seconds; `None` keeps all of it.
    #[must_use]
    pub fn keep_secs(self, kind: Kind) -> Option<i64> {
        match self {
            Self::Day if matches!(kind, Kind::Crypto | Kind::Future) => Some(DAY),
            Self::Day | Self::Week => None,
            Self::Month => Some(31 * DAY),
            Self::HalfYear => Some(183 * DAY),
            Self::Year => Some(366 * DAY),
            Self::FiveYears => Some(1_826 * DAY),
        }
    }

    /// Whether the bars are minutes (and so have sessions) rather than days.
    #[must_use]
    pub fn intraday(self) -> bool {
        matches!(self, Self::Day | Self::Week)
    }

    /// How often the open chart is fetched again.
    #[must_use]
    pub fn refresh_ms(self) -> u32 {
        match self {
            Self::Day => 30_000,
            Self::Week => 60_000,
            _ => 300_000,
        }
    }
}

/// The request for a symbol's bars.
#[must_use]
pub fn url(query: &str, frame: Frame) -> String {
    let mut out = String::from("https://ts-api.cnbc.com/harmony/app/charts/");
    out.push_str(frame.range());
    out.push_str(".json?symbol=");
    out.push_str(&crate::symbols::encode(query));
    out
}

/// One bar of the reply, reduced to what the chart needs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RawBar {
    pub ms: i64,
    /// The exchange's own clock for it, as seconds from 1970-01-01 00:00 of that clock.
    pub local: i64,
    pub close: f64,
}

/// Seconds on the exchange's clock from "20261123040000".
#[must_use]
pub fn trade_time(text: &str) -> Option<i64> {
    let digits = |from: usize, to: usize| -> Option<i64> { text.get(from..to)?.parse::<i64>().ok() };
    let (year, month, day) = (digits(0, 4)?, digits(4, 6)?, digits(6, 8)?);
    let (hour, minute, second) = (digits(8, 10)?, digits(10, 12)?, digits(12, 14)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    Some(days_from_civil(year, month, day) * DAY + hour * HOUR + minute * 60 + second)
}

/// Indices of up to `max` bars spread evenly over `start..n`, the first and the last among them.
#[must_use]
pub fn pick(start: usize, n: usize, max: usize) -> Vec<usize> {
    if n == 0 || start >= n || max == 0 {
        return Vec::new();
    }
    let count = n - start;
    if count <= max || max == 1 {
        return if count <= max { (start..n).collect() } else { vec![n - 1] };
    }
    (0..max).map(|k| start + k * (count - 1) / (max - 1)).collect()
}

/// The first bar within `keep_secs` of the last one, found by bisection; `time_of(i)` is bar `i`'s time in ms.
#[must_use]
pub fn start_index(n: usize, time_of: impl Fn(usize) -> i64, keep_secs: Option<i64>) -> usize {
    let Some(keep) = keep_secs else { return 0 };
    if n == 0 {
        return 0;
    }
    let cutoff = time_of(n - 1) - keep * 1_000;
    let (mut lo, mut hi) = (0, n - 1);
    while lo < hi {
        let mid = usize::midpoint(lo, hi);
        if time_of(mid) < cutoff {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Pre,
    Regular,
    Post,
}

/// Which part of the trading day a bar is in. Only stocks have a pre-market and an after-hours.
#[must_use]
pub fn phase_of(local: i64, kind: Kind, frame: Frame) -> Phase {
    if kind != Kind::Stock || !frame.intraday() {
        return Phase::Regular;
    }
    let secs = local.rem_euclid(DAY);
    if secs < OPEN {
        Phase::Pre
    } else if secs >= CLOSE {
        Phase::Post
    } else {
        Phase::Regular
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub ms: i64,
    pub local: i64,
    pub close: f64,
    pub phase: Phase,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Series {
    pub frame: Frame,
    pub kind: Kind,
    pub points: Vec<Point>,
}

impl Series {
    #[must_use]
    pub fn from_bars(frame: Frame, kind: Kind, bars: &[RawBar]) -> Self {
        let points = bars
            .iter()
            .map(|b| Point { ms: b.ms, local: b.local, close: b.close, phase: phase_of(b.local, kind, frame) })
            .collect();
        Self { frame, kind, points }
    }

    /// The points with the live price as the last one, when it is newer than the last bar.
    #[must_use]
    pub fn with_live(&self, live: Option<f64>, now_ms: i64, phase: Phase) -> Vec<Point> {
        let mut points = self.points.clone();
        if let (Some(price), Some(last)) = (live, points.last().copied())
            && now_ms > last.ms + 1_000 {
                points.push(Point { ms: now_ms, local: last.local + (now_ms - last.ms) / 1_000, close: price, phase });
            }
        points
    }
}

// ── Where things go ────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub struct Band {
    pub x0: f32,
    pub x1: f32,
    pub phase: Phase,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tick {
    pub at: f32,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Plot {
    /// The line, in pixels of the plotting box, y down.
    pub line: Vec<(f32, f32)>,
    pub bands: Vec<Band>,
    pub x_ticks: Vec<Tick>,
    /// Pixel row and price of each horizontal grid line.
    pub y_ticks: Vec<(f32, f64)>,
    /// The previous close, as a pixel row.
    pub reference: Option<f32>,
    pub lo: f64,
    pub hi: f64,
    /// The last price is at or above where the frame started (the previous close for a day).
    pub up: bool,
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Parts {
    year: i64,
    month: usize,
    day: u32,
    hour: u32,
    minute: u32,
    weekday: usize,
}

/// A calendar date from days since 1970-01-01 (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, usize, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let month = usize::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn parts(local: i64) -> Parts {
    let days = local.div_euclid(DAY);
    let secs = local.rem_euclid(DAY);
    let (year, month, day) = civil_from_days(days);
    Parts {
        year,
        month,
        day,
        hour: u32::try_from(secs / HOUR).unwrap_or(0),
        minute: u32::try_from(secs % HOUR / 60).unwrap_or(0),
        weekday: usize::try_from((days + 4).rem_euclid(7)).unwrap_or(0),
    }
}

fn push_int(out: &mut String, mut n: u64) {
    let mut digits = [0_u8; 20];
    let mut len = 0;
    loop {
        digits[len] = b'0' + u8::try_from(n % 10).unwrap_or(0);
        len += 1;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    for d in digits[..len].iter().rev() {
        out.push(char::from(*d));
    }
}

fn clock_label(local: i64) -> String {
    let p = parts(local);
    let mut out = String::new();
    if p.hour < 10 {
        out.push('0');
    }
    push_int(&mut out, u64::from(p.hour));
    out.push(':');
    if p.minute < 10 {
        out.push('0');
    }
    push_int(&mut out, u64::from(p.minute));
    out
}

fn date_label(local: i64) -> String {
    let p = parts(local);
    let mut out = String::from(MONTHS[(p.month - 1) % 12]);
    out.push(' ');
    push_int(&mut out, u64::from(p.day));
    out
}

fn year_label(local: i64) -> String {
    let mut out = String::new();
    push_int(&mut out, u64::try_from(parts(local).year).unwrap_or(0));
    out
}

/// Round numbers for the grid lines: steps of 1, 2 or 5 times a power of ten.
fn nice_step(range: f64, wanted: f64) -> f64 {
    let raw = range / wanted;
    let magnitude = 10_f64.powf(raw.log10().floor());
    let unit = raw / magnitude;
    let nice = if unit < 1.5 {
        1.0
    } else if unit < 3.0 {
        2.0
    } else if unit < 7.0 {
        5.0
    } else {
        10.0
    };
    nice * magnitude
}

/// The chart's picture for `points`, in a plotting box `w` by `h` pixels.
///
/// `reference` is the previous close, drawn as a dashed line on a day chart.
#[must_use]
#[expect(clippy::cast_precision_loss, clippy::cast_possible_truncation, reason = "pixel arithmetic")]
pub fn plot(points: &[Point], frame: Frame, kind: Kind, reference: Option<f64>, w: f32, h: f32) -> Option<Plot> {
    if points.len() < 2 || w <= 0.0 || h <= 0.0 {
        return None;
    }
    let (first, last) = (points[0], points[points.len() - 1]);
    let reference = reference.filter(|_| frame == Frame::Day);

    // The price range, with some air above and below.
    let mut lo = points.iter().map(|p| p.close).fold(f64::INFINITY, f64::min);
    let mut hi = points.iter().map(|p| p.close).fold(f64::NEG_INFINITY, f64::max);
    if let Some(r) = reference {
        lo = lo.min(r);
        hi = hi.max(r);
    }
    let air = if hi > lo { (hi - lo) * 0.08 } else { hi.abs().max(1.0) * 0.005 };
    let (lo, hi) = (lo - air, hi + air);
    let y_of = |price: f64| -> f32 { h - ((price - lo) / (hi - lo) * f64::from(h)) as f32 };

    // Where each point goes along the bottom.
    let day_start = last.local.div_euclid(DAY) * DAY;
    let session = match (frame, kind) {
        (Frame::Day, Kind::Stock) => Some((day_start + PRE_OPEN, day_start + POST_CLOSE)),
        (Frame::Day, Kind::Index) => Some((day_start + OPEN, day_start + CLOSE)),
        _ => None,
    };
    let n = points.len();
    let x_of = |i: usize, p: &Point| -> f32 {
        let fraction = match (session, frame) {
            (Some((from, to)), _) => (p.local - from) as f64 / (to - from) as f64,
            (None, Frame::Week) => i as f64 / (n - 1) as f64,
            (None, _) => (p.ms - first.ms) as f64 / (last.ms - first.ms).max(1) as f64,
        };
        (fraction.clamp(0.0, 1.0) * f64::from(w)) as f32
    };
    let line: Vec<(f32, f32)> = points.iter().enumerate().map(|(i, p)| (x_of(i, p), y_of(p.close))).collect();

    // Pre-market and after-hours stretches.
    let mut bands = Vec::new();
    if kind == Kind::Stock && frame.intraday() {
        if let (Some((from, to)), Frame::Day) = (session, frame) {
            let at = |secs: i64| (((secs - from) as f64 / (to - from) as f64) * f64::from(w)) as f32;
            bands.push(Band { x0: 0.0, x1: at(day_start + OPEN), phase: Phase::Pre });
            bands.push(Band { x0: at(day_start + CLOSE), x1: w, phase: Phase::Post });
        } else {
            let mut start = 0;
            for i in 1..=n {
                if i == n || points[i].phase != points[start].phase {
                    if points[start].phase != Phase::Regular {
                        let x1 = if i == n { line[n - 1].0 } else { f32::midpoint(line[i - 1].0, line[i].0) };
                        let x0 = if start == 0 { 0.0 } else { f32::midpoint(line[start - 1].0, line[start].0) };
                        bands.push(Band { x0, x1, phase: points[start].phase });
                    }
                    start = i;
                }
            }
        }
    }

    // Labels along the bottom.
    let mut x_ticks = Vec::new();
    match frame {
        Frame::Day => match session {
            Some((from, to)) => {
                let marks: &[i64] = if kind == Kind::Stock {
                    &[PRE_OPEN, OPEN, 12 * HOUR, CLOSE, POST_CLOSE]
                } else {
                    &[OPEN, 12 * HOUR, 14 * HOUR, CLOSE]
                };
                for m in marks {
                    let at = (((day_start + m - from) as f64 / (to - from) as f64) * f64::from(w)) as f32;
                    x_ticks.push(Tick { at, label: clock_label(day_start + m) });
                }
            }
            None => {
                for k in 0..4_i64 {
                    let local = first.local + (last.local - first.local) * k / 3;
                    x_ticks.push(Tick { at: w * k as f32 / 3.0, label: clock_label(local) });
                }
            }
        },
        Frame::Week => {
            let mut start = 0;
            for i in 1..=n {
                if i == n || points[i].local.div_euclid(DAY) != points[start].local.div_euclid(DAY) {
                    let at = f32::midpoint(line[start].0, line[i - 1].0);
                    x_ticks.push(Tick { at, label: String::from(WEEKDAYS[parts(points[start].local).weekday]) });
                    start = i;
                }
            }
        }
        Frame::Month => {
            for k in 0..5_usize {
                let i = (n - 1) * k / 4;
                x_ticks.push(Tick { at: line[i].0, label: date_label(points[i].local) });
            }
        }
        Frame::HalfYear | Frame::Year => {
            let mut months = Vec::new();
            for i in 1..n {
                if parts(points[i].local).month != parts(points[i - 1].local).month {
                    months.push(i);
                }
            }
            let step = months.len().div_ceil(8).max(1);
            for &i in months.iter().step_by(step) {
                x_ticks.push(Tick { at: line[i].0, label: String::from(MONTHS[(parts(points[i].local).month - 1) % 12]) });
            }
        }
        Frame::FiveYears => {
            for i in 1..n {
                if parts(points[i].local).year != parts(points[i - 1].local).year {
                    x_ticks.push(Tick { at: line[i].0, label: year_label(points[i].local) });
                }
            }
        }
    }

    // Horizontal grid lines at round prices.
    let step = nice_step(hi - lo, 4.0);
    let mut y_ticks = Vec::new();
    let mut price = (lo / step).ceil() * step;
    while price < hi && y_ticks.len() < 8 {
        y_ticks.push((y_of(price), price));
        price += step;
    }

    let baseline = reference.unwrap_or(first.close);
    Some(Plot {
        line,
        bands,
        x_ticks,
        y_ticks,
        reference: reference.map(y_of),
        lo,
        hi,
        up: last.close >= baseline,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(ms: i64, local: i64, close: f64) -> RawBar {
        RawBar { ms, local, close }
    }

    #[test]
    fn frames_name_the_range_that_holds_them() {
        assert_eq!(Frame::Day.range(), "1D");
        assert_eq!(Frame::Week.range(), "5D");
        for f in [Frame::Month, Frame::HalfYear, Frame::Year] {
            assert_eq!(f.range(), "1M");
        }
        assert_eq!(Frame::FiveYears.range(), "5Y");
        assert_eq!(Frame::Week.label(Kind::Stock), "5D");
        assert_eq!(Frame::Week.label(Kind::Crypto), "2D");
    }

    #[test]
    fn the_request_encodes_the_symbol() {
        assert_eq!(url("NVDA", Frame::Day), "https://ts-api.cnbc.com/harmony/app/charts/1D.json?symbol=NVDA");
        assert_eq!(url("BTC.CM=", Frame::Week), "https://ts-api.cnbc.com/harmony/app/charts/5D.json?symbol=BTC.CM%3D");
        assert_eq!(url("@GC.1", Frame::Year), "https://ts-api.cnbc.com/harmony/app/charts/1M.json?symbol=%40GC.1");
    }

    #[test]
    fn trade_times_become_seconds_on_the_exchange_clock() {
        // 2026-11-23 is 20_780 days after 1970-01-01.
        assert_eq!(trade_time("20261123040000"), Some(20_780 * DAY + 4 * HOUR));
        assert_eq!(trade_time("20261123093000"), Some(20_780 * DAY + OPEN));
        assert_eq!(trade_time("2026"), None);
        assert_eq!(trade_time("20261323040000"), None);
    }

    #[test]
    fn pick_spreads_the_bars_and_keeps_both_ends() {
        assert_eq!(pick(0, 5, 10), vec![0, 1, 2, 3, 4]);
        let few = pick(0, 1000, 5);
        assert_eq!(few.len(), 5);
        assert_eq!((few[0], few[4]), (0, 999));
        assert!(few.windows(2).all(|w| w[0] < w[1]));
        let tail = pick(900, 1000, 3);
        assert_eq!((tail[0], tail[2]), (900, 999));
        assert!(pick(5, 5, 3).is_empty());
        assert_eq!(pick(0, 100, 1), vec![99]);
    }

    #[test]
    fn start_index_finds_the_first_bar_in_the_window() {
        let times: Vec<i64> = (0..100).map(|i| i * 86_400_000).collect();
        let at = |i: usize| times[i];
        assert_eq!(start_index(100, at, None), 0);
        assert_eq!(start_index(100, at, Some(31 * DAY)), 68);
        assert_eq!(start_index(100, at, Some(1_000 * DAY)), 0);
        assert_eq!(start_index(0, at, Some(DAY)), 0);
    }

    #[test]
    fn only_stocks_have_sessions_and_only_on_minute_bars() {
        let pre = 20_780 * DAY + 5 * HOUR;
        let regular = 20_780 * DAY + 12 * HOUR;
        let post = 20_780 * DAY + 17 * HOUR;
        assert_eq!(phase_of(pre, Kind::Stock, Frame::Day), Phase::Pre);
        assert_eq!(phase_of(regular, Kind::Stock, Frame::Week), Phase::Regular);
        assert_eq!(phase_of(post, Kind::Stock, Frame::Day), Phase::Post);
        assert_eq!(phase_of(pre, Kind::Crypto, Frame::Day), Phase::Regular);
        assert_eq!(phase_of(pre, Kind::Stock, Frame::Month), Phase::Regular);
    }

    #[test]
    fn dates_come_out_of_the_exchange_clock() {
        let p = parts(20_780 * DAY + 13 * HOUR + 5 * 60);
        assert_eq!((p.year, p.month, p.day, p.hour, p.minute), (2026, 11, 23, 13, 5));
        assert_eq!(WEEKDAYS[p.weekday], "Mon");
        assert_eq!(date_label(20_780 * DAY), "Nov 23");
        assert_eq!(clock_label(20_780 * DAY + 9 * HOUR + 30 * 60), "09:30");
        assert_eq!(year_label(20_780 * DAY), "2026");
    }

    #[test]
    fn nice_steps_are_one_two_or_five() {
        assert!((nice_step(10.0, 4.0) - 2.0).abs() < 1e-9);
        assert!((nice_step(7.0, 4.0) - 2.0).abs() < 1e-9);
        assert!((nice_step(1_000.0, 4.0) - 200.0).abs() < 1e-9);
        assert!((nice_step(0.9, 4.0) - 0.2).abs() < 1e-9);
    }

    fn day_series(kind: Kind) -> Vec<Point> {
        // Bars at 04:00, 09:30, 12:00, 16:00 and 19:00 on 2026-11-23.
        let day = 20_780 * DAY;
        let times = [4 * HOUR, OPEN, 12 * HOUR, CLOSE, 19 * HOUR];
        let closes = [100.0, 102.0, 101.0, 103.0, 104.0];
        let bars: Vec<RawBar> = times.iter().zip(closes).map(|(t, c)| bar((day + t) * 1000, day + t, c)).collect();
        Series::from_bars(Frame::Day, kind, &bars).points
    }

    #[test]
    fn a_day_chart_spans_four_to_twenty_with_bands_for_the_extended_hours() {
        let plot = plot(&day_series(Kind::Stock), Frame::Day, Kind::Stock, Some(100.5), 1600.0, 300.0).expect("BUG: enough points");
        assert!((plot.line[0].0 - 0.0).abs() < 0.5, "04:00 is the left edge");
        // 19:00 is 15 of the 16 hours across.
        assert!((plot.line[4].0 - 1500.0).abs() < 0.5);
        assert_eq!(plot.bands.len(), 2);
        assert_eq!(plot.bands[0].phase, Phase::Pre);
        assert!((plot.bands[0].x1 - 1600.0 * 5.5 / 16.0).abs() < 0.5);
        assert_eq!(plot.bands[1].phase, Phase::Post);
        assert!((plot.bands[1].x0 - 1600.0 * 12.0 / 16.0).abs() < 0.5);
        let labels: Vec<&str> = plot.x_ticks.iter().map(|t| t.label.as_str()).collect();
        assert_eq!(labels, ["04:00", "09:30", "12:00", "16:00", "20:00"]);
        assert!(plot.reference.is_some());
        assert!(plot.up);
    }

    #[test]
    fn higher_prices_are_nearer_the_top() {
        let plot = plot(&day_series(Kind::Stock), Frame::Day, Kind::Stock, None, 1600.0, 300.0).expect("BUG: enough points");
        let ys: Vec<f32> = plot.line.iter().map(|p| p.1).collect();
        assert!(ys[4] < ys[0], "104 is above 100: {ys:?}");
        assert!(ys.iter().all(|y| (0.0..=300.0).contains(y)));
        for (y, price) in &plot.y_ticks {
            assert!((0.0..=300.0).contains(y) && *price > plot.lo && *price < plot.hi);
        }
    }

    #[test]
    fn a_coin_has_no_bands_and_labels_by_its_own_clock() {
        let plot = plot(&day_series(Kind::Crypto), Frame::Day, Kind::Crypto, None, 1000.0, 200.0).expect("BUG: enough points");
        assert!(plot.bands.is_empty());
        assert_eq!(plot.x_ticks.len(), 4);
        assert!((plot.line[0].0 - 0.0).abs() < 0.5 && (plot.line[4].0 - 1000.0).abs() < 0.5);
    }

    #[test]
    fn a_flat_series_still_has_a_range() {
        let flat: Vec<Point> = (0..5).map(|i| Point { ms: i * 60_000, local: 20_780 * DAY + i * 60, close: 50.0, phase: Phase::Regular }).collect();
        let plot = plot(&flat, Frame::Month, Kind::Stock, None, 800.0, 200.0).expect("BUG: enough points");
        assert!(plot.hi > plot.lo);
        assert!(plot.line.iter().all(|p| p.1.is_finite()));
    }

    #[test]
    fn a_month_has_five_date_labels_and_a_year_has_month_names() {
        let day = |i: i64| 20_500 * DAY + i * DAY;
        let bars: Vec<RawBar> = (0..250).map(|i| bar(day(i) * 1000, day(i), 100.0 + i as f64 * 0.1)).collect();
        let year = Series::from_bars(Frame::Year, Kind::Stock, &bars);
        let p = plot(&year.points, Frame::Year, Kind::Stock, None, 1000.0, 300.0).expect("BUG: enough points");
        assert!((4..=8).contains(&p.x_ticks.len()), "{:?}", p.x_ticks);
        assert!(p.x_ticks.iter().all(|t| MONTHS.contains(&t.label.as_str())));
        let month = plot(&year.points[200..], Frame::Month, Kind::Stock, None, 1000.0, 300.0).expect("BUG: enough points");
        assert_eq!(month.x_ticks.len(), 5);
        assert!(month.up);
    }

    #[test]
    fn five_days_show_the_weekdays_and_close_the_gaps_between_sessions() {
        let mut bars = Vec::new();
        for d in 0..5_i64 {
            for k in 0..10_i64 {
                // Mondays to Fridays of 2026-11-16, ten bars each, a night between them.
                let local = (20_773 + d) * DAY + 10 * HOUR + k * 600;
                bars.push(bar(local * 1000, local, 100.0 + (d * 10 + k) as f64));
            }
        }
        let series = Series::from_bars(Frame::Week, Kind::Stock, &bars);
        let p = plot(&series.points, Frame::Week, Kind::Stock, None, 1000.0, 300.0).expect("BUG: enough points");
        let labels: Vec<&str> = p.x_ticks.iter().map(|t| t.label.as_str()).collect();
        assert_eq!(labels, ["Mon", "Tue", "Wed", "Thu", "Fri"]);
        // Evenly spaced by position, whatever the nights in between.
        let step = p.line[1].0 - p.line[0].0;
        assert!(p.line.windows(2).all(|w| ((w[1].0 - w[0].0) - step).abs() < 0.01));
    }

    #[test]
    fn the_live_price_extends_the_series_only_when_it_is_newer() {
        let series = Series::from_bars(Frame::Day, Kind::Stock, &[bar(1_000_000, 1000, 10.0), bar(1_060_000, 1060, 11.0)]);
        assert_eq!(series.with_live(Some(12.0), 1_060_500, Phase::Regular).len(), 2);
        let longer = series.with_live(Some(12.0), 1_120_000, Phase::Regular);
        assert_eq!(longer.len(), 3);
        assert!((longer[2].close - 12.0).abs() < 1e-9);
        assert_eq!(longer[2].local, 1120);
        assert_eq!(series.with_live(None, 1_120_000, Phase::Regular).len(), 2);
    }
}
