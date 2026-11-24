//! Hyperliquid, the other place the prices can come from: a market that trades round the clock, stocks
//! and commodities included, with prices pushed over a WebSocket and candles on request.
//!
//! The main market has the crypto perpetuals (`BTC`, `ETH`, `kPEPE`); the markets that builders run on it
//! write their coins `dex:NAME`, such as `xyz:NVDA` and `xyz:GOLD`. This module is the plain part: the
//! messages to send, what a reply says, and the candles to ask for. The glue sends and reads them.

use crate::chart::{Frame, push_int};

/// The WebSocket is opened as a TLS socket on this host, and the upgrade asked for at this path: the Deck's
/// own WebSocket call is built without TLS. See `ws`.
pub const HOST: &str = "api.hyperliquid.xyz";
pub const WS_PATH: &str = "/ws";
pub const INFO_URL: &str = "https://api.hyperliquid.xyz/info";
/// The server closes a connection that says nothing for a minute.
pub const PING: &str = r#"{"method":"ping"}"#;
/// How long the connection may be silent before it is taken for dead, in milliseconds. A market sends
/// about once a second.
pub const SILENT_MS: u64 = 20_000;
/// How often to ping, in milliseconds.
pub const PING_EVERY_MS: u64 = 25_000;
/// How long a market that was subscribed to may stay without a price before it is asked of CNBC instead.
pub const PATIENCE_MS: u64 = 15_000;

/// A coin name that can go into a JSON text as it is.
fn plain(coin: &str) -> bool {
    !coin.is_empty() && coin.len() <= 40 && coin.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'_' | b'.' | b'-'))
}

/// The message that asks for a market's live context: its prices, its volume, the price a day ago.
#[must_use]
pub fn subscribe(coin: &str) -> Option<String> {
    plain(coin).then(|| {
        let mut out = String::from(r#"{"method":"subscribe","subscription":{"type":"activeAssetCtx","coin":""#);
        out.push_str(coin);
        out.push_str(r#""}}"#);
        out
    })
}

/// The coin that a refusal is about: the server answers a name it does not know with
/// `Invalid subscription {"type":"activeAssetCtx","coin":"NVDA"}`.
#[must_use]
pub fn refused_coin(error: &str) -> Option<String> {
    // The refusal is text inside a JSON string, so the quotes around the name may come escaped (`\"`).
    let at = error.find("coin")? + "coin".len();
    let rest = error.get(at..)?.trim_start_matches(['\\', '"', ':', ' ']);
    let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '.' | '-'))).unwrap_or(rest.len());
    let coin = rest.get(..end)?;
    plain(coin).then(|| coin.to_string())
}

/// What a market's context says, as the text the server writes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ctx {
    pub mark: String,
    pub mid: String,
    pub prev_day: String,
    /// The volume of the last day, in dollars.
    pub volume: String,
}

/// What the server sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    /// The prices of a market.
    Ctx { coin: String, ctx: Ctx },
    /// A subscription it does not have a market for: the coin it was about.
    Refused(String),
    /// Anything else: confirmations, pongs.
    Other,
}

/// The text of the string value that follows `needle`, which ends with the opening quote of the value.
fn after<'a>(text: &'a str, needle: &str) -> Option<&'a str> {
    let at = text.find(needle)? + needle.len();
    let rest = text.get(at..)?;
    rest.get(..rest.find('"')?)
}

/// Reads one message of the socket. The server writes them flat and alike, with the prices as strings,
/// so a scan for the few names that matter does it without parsing the whole text.
#[must_use]
pub fn parse(text: &str) -> Message {
    if text.contains(r#""channel":"activeAssetCtx""#) {
        let Some(coin) = after(text, r#""coin":""#).filter(|c| plain(c)) else { return Message::Other };
        let field = |needle: &str| after(text, needle).unwrap_or_default().to_string();
        let ctx = Ctx {
            mark: field(r#""markPx":""#),
            mid: field(r#""midPx":""#),
            prev_day: field(r#""prevDayPx":""#),
            volume: field(r#""dayNtlVlm":""#),
        };
        return Message::Ctx { coin: coin.to_string(), ctx };
    }
    if text.contains(r#""channel":"error""#) {
        return refused_coin(text).map_or(Message::Other, Message::Refused);
    }
    Message::Other
}

/// The candles of a `candleSnapshot` reply, oldest first: the time each opened, in milliseconds, and the
/// price it closed at. The reply is a list of flat objects with their prices as strings, so a scan for
/// the two fields that matter reads it without parsing the whole text, and the same way wherever it runs.
#[must_use]
pub fn candles(text: &str) -> Vec<(i64, f64)> {
    text.split(r#"{"t":"#)
        .skip(1)
        .filter_map(|piece| {
            let time = piece.split(',').next()?.trim().parse::<i64>().ok()?;
            let close = after(piece, r#""c":""#)?.parse::<f64>().ok()?;
            Some((time, close))
        })
        .collect()
}

/// The candles to ask for in a time frame: their size, and how far back to go in seconds.
#[must_use]
pub fn candles_for(frame: Frame) -> (&'static str, i64) {
    const DAY: i64 = 86_400;
    match frame {
        Frame::Day => ("5m", DAY),
        Frame::Week => ("30m", 5 * DAY),
        Frame::Month => ("4h", 31 * DAY),
        Frame::HalfYear => ("1d", 183 * DAY),
        Frame::Year => ("1d", 366 * DAY),
        Frame::FiveYears => ("1w", 1_826 * DAY),
    }
}

/// The request for a coin's candles over a time frame, ending now.
#[must_use]
pub fn candle_body(coin: &str, frame: Frame, now_ms: i64) -> Option<String> {
    if !plain(coin) || now_ms <= 0 {
        return None;
    }
    let (interval, span) = candles_for(frame);
    let mut out = String::from(r#"{"type":"candleSnapshot","req":{"coin":""#);
    out.push_str(coin);
    out.push_str(r#"","interval":""#);
    out.push_str(interval);
    out.push_str(r#"","startTime":"#);
    push_int(&mut out, u64::try_from((now_ms - span * 1_000).max(0)).unwrap_or(0));
    out.push_str(r#","endTime":"#);
    push_int(&mut out, u64::try_from(now_ms).unwrap_or(0));
    out.push_str("}}");
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_subscription_names_the_coin() {
        assert_eq!(
            subscribe("xyz:NVDA").as_deref(),
            Some(r#"{"method":"subscribe","subscription":{"type":"activeAssetCtx","coin":"xyz:NVDA"}}"#)
        );
        assert_eq!(subscribe("BTC").as_deref().map(str::len), Some(r#"{"method":"subscribe","subscription":{"type":"activeAssetCtx","coin":"BTC"}}"#.len()));
        assert_eq!(subscribe(r#"BTC"}"#), None, "a name that could break the message is not sent");
        assert_eq!(subscribe(""), None);
    }

    #[test]
    fn a_reply_of_candles_gives_each_opening_time_and_close() {
        let text = r#"[{"t":1795419600000,"T":1795419899999,"s":"xyz:NVDA","i":"5m","o":"134.96","c":"134.89","h":"135.14","l":"134.86","v":"1910.257","n":569},{"t":1795419900000,"T":1795420199999,"s":"xyz:NVDA","i":"5m","o":"134.91","c":"134.61","h":"135.05","l":"134.52","v":"4622.317","n":601}]"#;
        assert_eq!(candles(text), [(1_795_419_600_000, 134.89), (1_795_419_900_000, 134.61)]);
    }

    #[test]
    fn a_reply_that_is_not_candles_gives_none() {
        assert!(candles("null").is_empty());
        assert!(candles("[]").is_empty());
        assert!(candles("<html>Bad gateway</html>").is_empty());
        // A candle without a close, or with a time that is not a number, is left out.
        let text = r#"[{"t":1,"o":"1.0"},{"t":"x","c":"2.0"},{"t":3,"c":"3.5"}]"#;
        assert_eq!(candles(text), [(3, 3.5)]);
    }

    #[test]
    fn a_message_of_prices_is_read_whole() {
        let text = r#"{"channel":"activeAssetCtx","data":{"coin":"xyz:NVDA","ctx":{"funding":"0.00000625","openInterest":"4219.0","prevDayPx":"134.51","dayNtlVlm":"1615050662.07","premium":"0.0","oraclePx":"163.4","markPx":"163.43","midPx":"163.435","impactPxs":["163.4","163.47"],"dayBaseVlm":"9990243.0"}}}"#;
        assert_eq!(
            parse(text),
            Message::Ctx {
                coin: "xyz:NVDA".into(),
                ctx: Ctx { mark: "163.43".into(), mid: "163.435".into(), prev_day: "134.51".into(), volume: "1615050662.07".into() }
            }
        );
    }

    #[test]
    fn a_missing_mid_is_left_empty_and_a_message_without_a_coin_is_ignored() {
        let text = r#"{"channel":"activeAssetCtx","data":{"coin":"BTC","ctx":{"prevDayPx":"61521.0","dayNtlVlm":"4817910048.4","markPx":"60317.0","midPx":null}}}"#;
        let Message::Ctx { ctx, .. } = parse(text) else { panic!("BUG: a message of prices") };
        assert_eq!((ctx.mark.as_str(), ctx.mid.as_str()), ("60317.0", ""));
        assert_eq!(parse(r#"{"channel":"activeAssetCtx","data":{}}"#), Message::Other);
    }

    #[test]
    fn a_refusal_is_read_as_the_server_writes_it() {
        // Inside the JSON text the quotes of the refused request come escaped.
        let text = r#"{"channel":"error","data":"Invalid subscription {\"type\":\"activeAssetCtx\",\"coin\":\"JPM\"}"}"#;
        assert_eq!(parse(text), Message::Refused("JPM".into()));
        let builder = r#"{"channel":"error","data":"Invalid subscription {\"type\":\"activeAssetCtx\",\"coin\":\"xyz:JPM\"}"}"#;
        assert_eq!(parse(builder), Message::Refused("xyz:JPM".into()));
        assert_eq!(parse(r#"{"channel":"error","data":"Something else"}"#), Message::Other);
    }

    #[test]
    fn confirmations_and_pongs_say_nothing() {
        assert_eq!(parse(r#"{"channel":"pong"}"#), Message::Other);
        let ack = r#"{"channel":"subscriptionResponse","data":{"method":"subscribe","subscription":{"type":"activeAssetCtx","coin":"BTC"}}}"#;
        assert_eq!(parse(ack), Message::Other);
        assert_eq!(parse("not json"), Message::Other);
    }

    #[test]
    fn a_refusal_names_the_coin() {
        let error = r#"Invalid subscription {"type":"activeAssetCtx","coin":"NVDA"}"#;
        assert_eq!(refused_coin(error).as_deref(), Some("NVDA"));
        let builder = r#"Invalid subscription {"type":"activeAssetCtx","coin":"xyz:NOPE"}"#;
        assert_eq!(refused_coin(builder).as_deref(), Some("xyz:NOPE"));
        assert_eq!(refused_coin("something else"), None);
    }

    #[test]
    fn each_time_frame_asks_for_candles_that_fit_the_chart() {
        assert_eq!(candles_for(Frame::Day), ("5m", 86_400));
        // 24 hours of 5-minute candles, five days of half hours: a few hundred each, never thousands.
        for frame in Frame::ALL {
            let (interval, span) = candles_for(frame);
            let step = match interval {
                "5m" => 300,
                "30m" => 1_800,
                "4h" => 14_400,
                "1d" => 86_400,
                "1w" => 604_800,
                other => panic!("BUG: unexpected interval {other}"),
            };
            let count = span / step;
            assert!((24..=400).contains(&count), "{frame:?}: {count} candles");
        }
    }

    #[test]
    fn the_candle_request_spans_the_frame_up_to_now() {
        let body = candle_body("xyz:NVDA", Frame::Week, 1_795_000_000_000).expect("BUG: a plain coin");
        assert_eq!(
            body,
            r#"{"type":"candleSnapshot","req":{"coin":"xyz:NVDA","interval":"30m","startTime":1794568000000,"endTime":1795000000000}}"#
        );
        assert_eq!(candle_body("BTC\"", Frame::Day, 1_795_000_000_000), None);
        assert_eq!(candle_body("BTC", Frame::Day, 0), None);
    }
}
