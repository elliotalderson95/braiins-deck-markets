//! TradingView, for what neither Hyperliquid nor CNBC has: the economic indicators (the ISM Purchasing
//! Managers Index, inflation, jobs, the Fed's rate) and indexes such as the Cboe VIX. Its public quote
//! scanner answers a batch of tickers (`ECONOMICS:USBCOI`, `TVC:VIX`) with their latest value and change,
//! and its chart socket sends the bars of a symbol, which is where the history of an indicator comes from.
//!
//! This module is the plain part: the request bodies, what a reply says, the messages of the chart socket
//! and the bars turned into a chart's series. The glue sends and receives. Neither endpoint is documented
//! for this use and either can change or stop.

use crate::chart::{self, Frame, MAX_POINTS, RawBar, Series};
use crate::clock;
use crate::symbols::Kind;

pub const SCAN_URL: &str = "https://scanner.tradingview.com/global/scan";
/// The chart socket is opened as a TLS socket on this host, and upgraded at this path: see `ws`.
pub const HOST: &str = "data.tradingview.com";
pub const WS_PATH: &str = "/socket.io/websocket?from=chart%2F&type=chart";
/// The chart socket turns away a client that does not say where it comes from.
pub const ORIGIN: &str = "https://www.tradingview.com";
/// How long a chart may take to arrive before it is given up, in milliseconds.
pub const CHART_PATIENCE_MS: u64 = 20_000;

const DAY: i64 = 86_400;

/// A ticker that can go into a JSON text as it is: `EXCHANGE:NAME`.
#[must_use]
pub fn plain(ticker: &str) -> bool {
    ticker.len() <= 40
        && ticker.contains(':')
        && ticker.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'_' | b'.' | b'-' | b'!' | b'&' | b'^'))
}

/// The scanner request for a batch of tickers; `None` when there is none it can take.
#[must_use]
pub fn scan_body(tickers: &[String]) -> Option<String> {
    let good: Vec<&String> = tickers.iter().filter(|t| plain(t)).collect();
    if good.is_empty() {
        return None;
    }
    let mut out = String::from(r#"{"symbols":{"tickers":["#);
    for (i, ticker) in good.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(ticker);
        out.push('"');
    }
    out.push_str(r#"]},"columns":["description","close","change","change_abs","time","type","update_mode"]}"#);
    Some(out)
}

/// What the scanner says of one ticker.
#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    pub ticker: String,
    pub description: String,
    pub close: f64,
    /// The change since the previous value, in percent and in the value's own unit.
    pub change_pct: f64,
    pub change_abs: f64,
    /// Unix time of the value; for an indicator the end of the period it measures.
    pub time: i64,
    /// An economic indicator, which is released now and then, not traded.
    pub economic: bool,
    /// The value comes with a delay (15 minutes for the Cboe's own VIX).
    pub delayed: bool,
}

#[derive(Clone, Debug, PartialEq)]
enum Cell {
    Text(String),
    Num(f64),
    Null,
}

/// The cells of a JSON array, from just after its `[` to its `]`: strings, numbers and nulls.
fn cells(text: &str) -> Vec<Cell> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            ']' => break,
            ',' | ' ' | '\n' => {
                chars.next();
            }
            '"' => {
                chars.next();
                let mut s = String::new();
                while let Some(ch) = chars.next() {
                    match ch {
                        '"' => break,
                        '\\' => match chars.next() {
                            Some('n') => s.push('\n'),
                            Some('t') => s.push('\t'),
                            Some('u') => {
                                let code: String = chars.by_ref().take(4).collect();
                                s.push(u32::from_str_radix(&code, 16).ok().and_then(char::from_u32).unwrap_or('?'));
                            }
                            Some(other) => s.push(other),
                            None => break,
                        },
                        other => s.push(other),
                    }
                }
                out.push(Cell::Text(s));
            }
            _ => {
                let mut token = String::new();
                while let Some(&ch) = chars.peek() {
                    if ch == ',' || ch == ']' {
                        break;
                    }
                    token.push(ch);
                    chars.next();
                }
                out.push(token.trim().parse::<f64>().map_or(Cell::Null, Cell::Num));
            }
        }
    }
    out
}

/// The readings in a scanner reply. A ticker without a value is left out.
#[must_use]
pub fn readings(text: &str) -> Vec<Reading> {
    text.split(r#"{"s":""#)
        .skip(1)
        .filter_map(|piece| {
            let ticker = piece.get(..piece.find('"')?)?.to_string();
            let at = piece.find(r#""d":["#)? + r#""d":["#.len();
            let row = cells(piece.get(at..)?);
            let num = |i: usize| match row.get(i) {
                Some(Cell::Num(v)) => Some(*v),
                _ => None,
            };
            let text_at = |i: usize| match row.get(i) {
                Some(Cell::Text(s)) => s.clone(),
                _ => String::new(),
            };
            #[expect(clippy::cast_possible_truncation, reason = "a Unix time in seconds")]
            Some(Reading {
                ticker,
                description: text_at(0),
                close: num(1)?,
                change_pct: num(2).unwrap_or(0.0),
                change_abs: num(3).unwrap_or(0.0),
                time: num(4).map_or(0, |t| t as i64),
                economic: text_at(5) == "economic",
                delayed: text_at(6).starts_with("delayed"),
            })
        })
        .collect()
}

// ── The chart socket ───────────────────────────────────────────────

/// One message of the chart socket: `~m~<length>~m~<text>`.
#[must_use]
pub fn frame_message(payload: &str) -> String {
    format!("~m~{}~m~{}", payload.chars().count(), payload)
}

/// The messages in a frame from the server, without their length marks. A heartbeat is `~h~<n>`, to be
/// sent back as it is; the rest is JSON.
#[must_use]
pub fn split_messages(text: &str) -> Vec<&str> {
    text.split("~m~").enumerate().filter(|(i, part)| *i >= 2 && i % 2 == 0 && !part.is_empty()).map(|(_, part)| part).collect()
}

/// The messages that ask for the bars of a symbol: an anonymous session, the symbol and its series.
#[must_use]
pub fn chart_messages(session: &str, ticker: &str, interval: &str, count: u32) -> Vec<String> {
    vec![
        frame_message(r#"{"m":"set_auth_token","p":["unauthorized_user_token"]}"#),
        frame_message(&format!(r#"{{"m":"chart_create_session","p":["{session}",""]}}"#)),
        frame_message(&format!(
            r#"{{"m":"resolve_symbol","p":["{session}","sds_sym_1","={{\"symbol\":\"{ticker}\",\"adjustment\":\"splits\"}}"]}}"#
        )),
        frame_message(&format!(r#"{{"m":"create_series","p":["{session}","sds_1","s1","sds_sym_1","{interval}",{count},""]}}"#)),
    ]
}

/// What a message of the chart socket says.
#[derive(Clone, Debug, PartialEq)]
pub enum ChartEvent {
    /// Bars: the Unix time of each and its close.
    Bars(Vec<(i64, f64)>),
    /// All the bars asked for have come.
    Completed,
    /// The symbol or the series cannot be had.
    Failed,
    Other,
}

#[must_use]
pub fn chart_event(payload: &str) -> ChartEvent {
    if payload.contains(r#""m":"timescale_update""#) || payload.contains(r#""m":"du""#) {
        ChartEvent::Bars(bars_in(payload))
    } else if payload.contains(r#""m":"series_completed""#) {
        ChartEvent::Completed
    } else if ["symbol_error", "series_error", "critical_error", "protocol_error"].iter().any(|m| payload.contains(&format!(r#""m":"{m}""#))) {
        ChartEvent::Failed
    } else {
        ChartEvent::Other
    }
}

/// The bars of a message: each is `"v":[time,open,high,low,close,...]`.
fn bars_in(payload: &str) -> Vec<(i64, f64)> {
    payload
        .split(r#""v":["#)
        .skip(1)
        .filter_map(|piece| {
            let inside = piece.get(..piece.find(']')?)?;
            let mut parts = inside.split(',');
            let time = parts.next()?.trim().parse::<f64>().ok()?;
            let close = parts.nth(3)?.trim().parse::<f64>().ok()?;
            #[expect(clippy::cast_possible_truncation, reason = "a Unix time in seconds")]
            Some((time as i64, close))
        })
        .collect()
}

/// The bar size and how many bars to ask for, for a symbol's time frame. An indicator has monthly values.
#[must_use]
pub fn chart_request(kind: Kind, frame: Frame) -> (&'static str, u32) {
    if kind == Kind::Indicator {
        return ("M", 120);
    }
    match frame {
        Frame::Day => ("5", 300),
        Frame::Week => ("30", 300),
        Frame::Month => ("240", 150),
        Frame::HalfYear => ("D", 200),
        Frame::Year => ("D", 300),
        Frame::FiveYears => ("W", 300),
    }
}

/// How much of the bars to keep from the last one, in seconds; `None` keeps all.
fn keep_secs(kind: Kind, frame: Frame) -> Option<i64> {
    match frame {
        // An index's day is picked by its date instead.
        Frame::Day if kind == Kind::Index => None,
        Frame::Day => Some(DAY),
        Frame::Week => Some(5 * DAY),
        other => other.keep_secs(kind),
    }
}

/// The bars as a chart's series. Minutes carry New York's clock, as CNBC's do, so that an index's day
/// can be drawn from 9:30 to 16:00; days and months carry their date.
#[must_use]
pub fn series(bars: &[(i64, f64)], kind: Kind, frame: Frame) -> Option<Series> {
    const OPEN: i64 = 9 * 3_600 + 30 * 60;
    const CLOSE: i64 = 16 * 3_600;
    let mut bars: Vec<(i64, f64)> = bars.iter().copied().filter(|b| b.1.is_finite()).collect();
    bars.sort_by_key(|b| b.0);
    bars.dedup_by_key(|b| b.0);
    let local = |t: i64| if frame.intraday() && kind != Kind::Indicator { t + clock::new_york_offset(t) } else { t };
    if frame == Frame::Day && kind == Kind::Index {
        // The regular session of the last day that has one, as the index's own chart is.
        bars.retain(|b| (OPEN..CLOSE).contains(&local(b.0).rem_euclid(DAY)));
        if let Some(last) = bars.last().copied() {
            let day = local(last.0).div_euclid(DAY);
            bars.retain(|b| local(b.0).div_euclid(DAY) == day);
        }
    }
    let start = chart::start_index(bars.len(), |i| bars[i].0 * 1_000, keep_secs(kind, frame));
    let raw: Vec<RawBar> = chart::pick(start, bars.len(), MAX_POINTS)
        .into_iter()
        .filter_map(|i| bars.get(i))
        .map(|&(t, close)| RawBar { ms: t * 1_000, local: local(t), close })
        .collect();
    (raw.len() >= 2).then(|| Series::from_bars(frame, kind, &raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPLY: &str = r#"{"totalCount":4,"data":[{"s":"ECONOMICS:USBCOI","d":["United States ISM Purchasing Managers Index (PMI)",55.5,1.834862385321101,1,1793404800,"economic","streaming"]},{"s":"CBOE:VIX","d":["CBOE Volatility Index",28.66,7.66,2.04,1795508100,"index","delayed_streaming_900"]},{"s":"ECONOMICS:USUR","d":["United States Unemployment Rate",4.1,-2.38,-0.10000000000000053,1793404800,"economic","streaming"]},{"s":"X:ODD","d":["A, \"quoted\" name",null,1,2,3,"index",null]}]}"#;

    #[test]
    fn a_scan_asks_for_the_tickers_and_columns_it_reads() {
        let body = scan_body(&["ECONOMICS:USBCOI".into(), "TVC:VIX".into(), "bad\"ticker:X".into()]).expect("BUG: two good tickers");
        assert!(body.starts_with(r#"{"symbols":{"tickers":["ECONOMICS:USBCOI","TVC:VIX"]},"columns":["description","close","change","#));
        assert!(!body.contains("bad"));
        assert_eq!(scan_body(&[]), None);
        assert_eq!(scan_body(&["NVDA".into()]), None, "a ticker has an exchange");
    }

    #[test]
    fn a_scan_reply_is_read_ticker_by_ticker() {
        let all = readings(REPLY);
        assert_eq!(all.len(), 3, "the one without a value is left out");
        let ism = &all[0];
        assert_eq!(ism.ticker, "ECONOMICS:USBCOI");
        assert_eq!(ism.description, "United States ISM Purchasing Managers Index (PMI)");
        assert!((ism.close - 55.5).abs() < 1e-9 && (ism.change_abs - 1.0).abs() < 1e-9);
        assert_eq!(ism.time, 1_793_404_800);
        assert!(ism.economic && !ism.delayed);
        let vix = &all[1];
        assert!(!vix.economic && vix.delayed);
        assert!((vix.change_pct - 7.66).abs() < 1e-9);
        assert!((all[2].change_abs + 0.1).abs() < 1e-9);
    }

    #[test]
    fn strings_with_commas_quotes_and_escapes_keep_their_cells_apart() {
        let row = cells(r#""A, \"b\" A",1.5,null,"x"],"rest"#);
        assert_eq!(row, [Cell::Text(r#"A, "b" A"#.into()), Cell::Num(1.5), Cell::Null, Cell::Text("x".into())]);
    }

    #[test]
    fn a_message_is_framed_with_its_length() {
        assert_eq!(frame_message(r#"{"m":"x"}"#), r#"~m~9~m~{"m":"x"}"#);
        assert_eq!(frame_message("héllo"), "~m~5~m~héllo");
    }

    #[test]
    fn a_frame_from_the_server_splits_into_its_messages() {
        let text = r#"~m~5~m~~h~12~m~14~m~{"m":"a","p":1}~m~15~m~{"m":"b","p":22}"#;
        assert_eq!(split_messages(text), ["~h~12", r#"{"m":"a","p":1}"#, r#"{"m":"b","p":22}"#]);
        assert!(split_messages("").is_empty());
        assert!(split_messages("not a frame").is_empty());
    }

    #[test]
    fn the_chart_is_asked_for_in_four_messages() {
        let all = chart_messages("cs_abc", "ECONOMICS:USBCOI", "M", 120);
        assert_eq!(all.len(), 4);
        assert!(all[0].ends_with(r#"{"m":"set_auth_token","p":["unauthorized_user_token"]}"#));
        assert!(all[1].contains(r#""chart_create_session","p":["cs_abc",""]"#));
        assert!(all[2].contains(r#""p":["cs_abc","sds_sym_1","={\"symbol\":\"ECONOMICS:USBCOI\",\"adjustment\":\"splits\"}"]"#));
        assert!(all[3].contains(r#""create_series","p":["cs_abc","sds_1","s1","sds_sym_1","M",120,""]"#));
        // Each is framed with the length of what follows.
        for m in &all {
            let (len, body) = m.trim_start_matches("~m~").split_once("~m~").expect("BUG: framed");
            assert_eq!(len.parse::<usize>().ok(), Some(body.chars().count()));
        }
    }

    #[test]
    fn bars_completion_and_failures_are_told_apart() {
        let bars = r#"{"m":"timescale_update","p":["cs_x",{"sds_1":{"s":[{"i":0,"v":[1790812800,54.5,54.5,54.5,54.5]},{"i":1,"v":[1793491200,55.5,55.5,55.5,55.5,0]}],"ns":{"d":"","indexes":"nochange"},"t":"s1"}}]}"#;
        assert_eq!(chart_event(bars), ChartEvent::Bars(vec![(1_790_812_800, 54.5), (1_793_491_200, 55.5)]));
        assert_eq!(chart_event(r#"{"m":"series_completed","p":["cs_x","sds_1","streaming","s1",{"rt_update_period":1}]}"#), ChartEvent::Completed);
        assert_eq!(chart_event(r#"{"m":"symbol_error","p":["cs_x","sds_sym_1","invalid_symbol"]}"#), ChartEvent::Failed);
        assert_eq!(chart_event(r#"{"m":"quote_completed","p":[]}"#), ChartEvent::Other);
        assert_eq!(chart_event(r#"{"session_id":"x"}"#), ChartEvent::Other);
    }

    #[test]
    fn a_bar_with_a_missing_price_is_left_out() {
        let text = r#"{"m":"du","p":["cs_x",{"sds_1":{"s":[{"i":1,"v":[1.0,null,null,null,null]},{"i":2,"v":[2.5e3,1,2,0,3.5]}]}}]}"#;
        assert_eq!(chart_event(text), ChartEvent::Bars(vec![(2_500, 3.5)]));
    }

    #[test]
    fn an_indicator_asks_for_monthly_bars_and_the_rest_by_time_frame() {
        assert_eq!(chart_request(Kind::Indicator, Frame::Year), ("M", 120));
        assert_eq!(chart_request(Kind::Indicator, Frame::FiveYears), ("M", 120));
        assert_eq!(chart_request(Kind::Index, Frame::Day), ("5", 300));
        assert_eq!(chart_request(Kind::Index, Frame::FiveYears), ("W", 300));
    }

    fn months(n: i64) -> Vec<(i64, f64)> {
        // Monthly bars ending 2026-10-01 (20_727 days after 1970), 30 days apart.
        (0..n).map(|i| ((20_727 - 30 * (n - 1 - i)) * DAY, 50.0 + i as f64 / 4.0)).collect()
    }

    #[test]
    fn the_months_of_an_indicator_are_cut_to_the_time_frame() {
        let bars = months(120);
        let year = series(&bars, Kind::Indicator, Frame::Year).expect("BUG: a year of months");
        assert_eq!(year.points.len(), 13, "a year back from the last month, both ends");
        let five = series(&bars, Kind::Indicator, Frame::FiveYears).expect("BUG: five years of months");
        assert!((60..=62).contains(&five.points.len()), "{}", five.points.len());
        let half = series(&bars, Kind::Indicator, Frame::HalfYear).expect("BUG: six months");
        assert_eq!(half.points.len(), 7);
        assert_eq!(year.points.last().map(|p| p.close), bars.last().map(|b| b.1));
        assert_eq!(year.points[0].local, year.points[0].ms / 1_000, "months carry their date, not a clock");
        assert!(series(&bars[..1], Kind::Indicator, Frame::Year).is_none());
    }

    #[test]
    fn an_index_day_is_the_regular_session_of_the_last_day_by_new_york_time() {
        // Tuesday 2026-11-24 and Monday 2026-11-23, five-minute bars every hour of the clock; winter time is UTC-5.
        let day = |d: i64| days_from(d) * DAY;
        fn days_from(d: i64) -> i64 {
            20_780 + d
        }
        let mut bars = Vec::new();
        for d in [0, 1] {
            for hour in 0..24 {
                bars.push((day(d) + hour * 3_600, 20.0 + hour as f64));
            }
        }
        let series = series(&bars, Kind::Index, Frame::Day).expect("BUG: a session");
        // 14:00 to 20:00 UTC is 09:00 to 15:00 in New York: from 14:30 on, so 15:00 UTC to 20:00 UTC.
        let hours: Vec<i64> = series.points.iter().map(|p| p.local.rem_euclid(DAY) / 3_600).collect();
        assert_eq!(hours, [10, 11, 12, 13, 14, 15]);
        let days: std::collections::HashSet<i64> = series.points.iter().map(|p| p.local.div_euclid(DAY)).collect();
        assert_eq!(days.len(), 1, "one day");
    }

    #[test]
    fn minutes_of_another_symbol_carry_new_yorks_clock() {
        let t = 20_780 * DAY + 14 * 3_600;
        let bars: Vec<(i64, f64)> = (0..30).map(|i| (t + i * 300, 100.0 + i as f64)).collect();
        let s = series(&bars, Kind::Future, Frame::Day).expect("BUG: bars");
        assert_eq!(s.points[0].local - s.points[0].ms / 1_000, -5 * 3_600, "winter in New York");
    }
}
