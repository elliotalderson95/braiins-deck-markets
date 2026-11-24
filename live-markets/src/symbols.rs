//! The watchlist: what the operator typed, what CNBC's quote service calls it, and what Hyperliquid does.

/// The most symbols a list can hold. The manifest says the same.
pub const MAX_SYMBOLS: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Stock,
    Index,
    Crypto,
    Future,
    /// A Hyperliquid perpetual: it trades round the clock, so it has no sessions.
    Perp,
}

/// Where a symbol's prices come from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Source {
    #[default]
    Hyperliquid,
    Cnbc,
}

impl Source {
    /// The value of the `source` setting; anything but `cnbc` is Hyperliquid, the default.
    #[must_use]
    pub fn from_param(value: Option<&str>) -> Self {
        match value {
            Some(v) if v.trim().eq_ignore_ascii_case("cnbc") => Self::Cnbc,
            _ => Self::Hyperliquid,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// What the screen calls it: `NVDA`, `BTC`, `S&P 500`.
    pub label: String,
    /// Its symbol in the quote service: `NVDA`, `BTC.CM=`, `.SPX`.
    pub query: String,
    /// What it is on CNBC; a Hyperliquid perpetual is `Kind::Perp` whatever this says.
    pub kind: Kind,
    /// Its names on Hyperliquid, best first: `BTC`, `xyz:NVDA`. None for what only CNBC has.
    pub hl: Vec<String>,
}

/// The names people type for what the services write differently: CNBC's symbol, the label,
/// the kind, and Hyperliquid's coins for it.
fn alias(typed: &str) -> Option<(&'static str, &'static str, Kind, &'static [&'static str])> {
    Some(match typed {
        "^GSPC" | "GSPC" | "SPX" | ".SPX" | "S&P500" | "S&P" | "SP500" => (".SPX", "S&P 500", Kind::Index, &["xyz:SP500"]),
        "^IXIC" | "IXIC" | ".IXIC" | "NASDAQ" | "COMP" | "NASDAQCOMPOSITE" => (".IXIC", "NASDAQ", Kind::Index, &[]),
        "^DJI" | "DJI" | ".DJI" | "DOW" | "DJIA" => (".DJI", "DOW", Kind::Index, &[]),
        "^NDX" | "NDX" | ".NDX" | "NASDAQ100" => (".NDX", "NASDAQ 100", Kind::Index, &["xyz:XYZ100"]),
        "^RUT" | "RUT" | ".RUT" => (".RUT", "RUSSELL 2000", Kind::Index, &[]),
        "^VIX" | "VIX" | ".VIX" => (".VIX", "VIX", Kind::Index, &["xyz:VIX"]),
        "GOLD" | "XAU" | "XAU-USD" | "XAUUSD" | "GC=F" => ("@GC.1", "GOLD", Kind::Future, &["xyz:GOLD"]),
        "SILVER" | "XAG" | "XAG-USD" | "XAGUSD" | "SI=F" => ("@SI.1", "SILVER", Kind::Future, &["xyz:SILVER"]),
        "OIL" | "WTI" | "CL=F" | "CRUDE" => ("@CL.1", "OIL", Kind::Future, &["xyz:CL"]),
        "BRENT" | "BZ=F" => ("@LCO.1", "BRENT", Kind::Future, &["xyz:BRENTOIL"]),
        "BITCOIN" | "BTC" | "BTC-USD" | "BTCUSD" | "XBT" => ("BTC.CM=", "BTC", Kind::Crypto, &["BTC"]),
        "ETHEREUM" | "ETH" | "ETH-USD" | "ETHUSD" => ("ETH.CM=", "ETH", Kind::Crypto, &["ETH"]),
        _ => return None,
    })
}

/// What CNBC calls the commodities and indexes that a builder market lists under its own names, as the
/// typed name that `alias` knows them by: `xyz:CL` is crude oil, not the stock of Colgate-Palmolive.
fn builder_name(name: &str) -> &str {
    match name {
        "CL" => "OIL",
        "BRENTOIL" => "BRENT",
        "XYZ100" => "NASDAQ100",
        other => other,
    }
}

/// `SOL-USD` and the like: a coin against the dollar.
fn crypto_pair(typed: &str) -> Option<String> {
    let base = typed.strip_suffix("-USD")?;
    let fits = (2..=6).contains(&base.len()) && base.bytes().all(|b| b.is_ascii_alphanumeric());
    fits.then(|| base.to_string())
}

/// `xyz:NVDA`: a Hyperliquid coin as that exchange writes it, a market of one of its builders (lower case)
/// and a name. `cnbc:NVDA` asks for CNBC's quote instead.
fn prefixed(raw: &str) -> Option<(&str, &str)> {
    let (head, tail) = raw.split_once(':')?;
    let name = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'.';
    let head_fits = !head.is_empty() && head.len() <= 8 && head.bytes().all(name);
    // After `cnbc:` any symbol goes, such as BTC-USD or BRK/B.
    let tail_fits = !tail.is_empty() && tail.len() <= 24 && tail.bytes().all(|b| name(b) || matches!(b, b'-' | b'/' | b'^' | b'=' | b'&'));
    (head_fits && tail_fits).then_some((head, tail))
}

/// One list item as the services know it; `None` for a blank item.
///
/// A name it cannot place is kept as typed, so that the screen says "No data" for it
/// and a typo shows instead of vanishing.
pub fn resolve(raw: &str) -> Option<Entry> {
    let compact: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.is_empty() {
        return None;
    }
    if let Some((head, tail)) = prefixed(&compact) {
        let name = tail.to_uppercase();
        if head.eq_ignore_ascii_case("cnbc") {
            let mut entry = resolve_cnbc(&name)?;
            entry.hl.clear();
            return Some(entry);
        }
        // A coin exactly as Hyperliquid names it. CNBC has the commodities and indexes under other names, and
        // for the rest the plain ticker is its best guess.
        if name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.')) {
            let coin = format!("{}:{}", head.to_lowercase(), name);
            if let Some((query, label, kind, _)) = alias(builder_name(&name)) {
                return Some(Entry { label: label.to_string(), query: query.to_string(), kind, hl: vec![coin] });
            }
            return Some(Entry { label: name.clone(), query: name, kind: Kind::Stock, hl: vec![coin] });
        }
    }
    let mut entry = resolve_cnbc(&compact.to_uppercase())?;
    if entry.hl.is_empty() && entry.kind == Kind::Stock {
        // A plain ticker: a crypto perpetual of the main market, or a stock or commodity of the biggest builder's.
        if entry.query.bytes().all(|b| b.is_ascii_alphanumeric()) {
            entry.hl = vec![entry.query.clone(), format!("xyz:{}", entry.query)];
        }
    }
    Some(entry)
}

fn resolve_cnbc(typed: &str) -> Option<Entry> {
    if typed.is_empty() {
        return None;
    }
    if let Some((query, label, kind, hl)) = alias(typed) {
        return Some(Entry { label: label.to_string(), query: query.to_string(), kind, hl: hl.iter().map(|c| (*c).to_string()).collect() });
    }
    if let Some(base) = crypto_pair(typed) {
        // Hyperliquid writes the small coins of the main market with a k: kPEPE.
        let hl = vec![base.clone(), format!("k{base}")];
        return Some(Entry { query: fmt_coin(&base), label: base, kind: Kind::Crypto, hl });
    }
    // Share classes: the service writes BRK.B where people write BRK-B or BRK/B.
    let query = share_class(typed);
    Some(Entry { label: typed.to_string(), query, kind: Kind::Stock, hl: Vec::new() })
}

fn fmt_coin(base: &str) -> String {
    let mut s = String::with_capacity(base.len() + 4);
    s.push_str(base);
    s.push_str(".CM=");
    s
}

fn share_class(typed: &str) -> String {
    if let Some((head, tail)) = typed.rsplit_once(['-', '/'])
        && (1..=2).contains(&tail.len()) && tail.bytes().all(|b| b.is_ascii_alphabetic()) && !head.is_empty() {
            let mut s = String::with_capacity(typed.len());
            s.push_str(head);
            s.push('.');
            s.push_str(tail);
            return s;
        }
    typed.to_string()
}

/// The whole list: blanks and repeats dropped, at most `MAX_SYMBOLS`.
pub fn resolve_all<S: AsRef<str>>(items: &[S]) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    for item in items {
        if let Some(entry) = resolve(item.as_ref())
            && !out.iter().any(|e| e.query == entry.query) {
                out.push(entry);
            }
        if out.len() == MAX_SYMBOLS {
            break;
        }
    }
    out
}

/// A query component with what a URL reserves written as `%XX`.
pub fn encode(raw: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(raw.len() + 8);
    for b in raw.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(b >> 4)]));
            out.push(char::from(HEX[usize::from(b & 15)]));
        }
    }
    out
}

/// The `symbols` value of a quote request: every query, joined by `|`, encoded.
pub fn request_list(entries: &[Entry]) -> String {
    let mut out = String::new();
    for (i, entry) in entries.iter().enumerate() {
        if i > 0 {
            out.push_str("%7C");
        }
        out.push_str(&encode(&entry.query));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(s: &str) -> Entry {
        resolve(s).expect("BUG: test input is not blank")
    }

    #[test]
    fn plain_tickers_pass_through_in_capitals() {
        let e = one(" nvda ");
        assert_eq!((e.label.as_str(), e.query.as_str(), e.kind), ("NVDA", "NVDA", Kind::Stock));
    }

    #[test]
    fn blank_items_are_skipped() {
        assert_eq!(resolve("   "), None);
        assert_eq!(resolve(""), None);
    }

    #[test]
    fn indexes_use_the_services_names() {
        assert_eq!(one("^GSPC").query, ".SPX");
        assert_eq!(one("S&P 500").query, ".SPX");
        assert_eq!(one("nasdaq").label, "NASDAQ");
        assert_eq!(one("^DJI").kind, Kind::Index);
    }

    #[test]
    fn coins_become_the_services_crypto_symbols() {
        let btc = one("BTC-USD");
        assert_eq!((btc.label.as_str(), btc.query.as_str(), btc.kind), ("BTC", "BTC.CM=", Kind::Crypto));
        assert_eq!(one("bitcoin").query, "BTC.CM=");
        assert_eq!(one("sol-usd").query, "SOL.CM=");
        // A bare ticker that is not a coin stays a stock.
        assert_eq!(one("COIN").kind, Kind::Stock);
    }

    #[test]
    fn gold_is_marked_as_a_future() {
        let gold = one("XAU-USD");
        assert_eq!((gold.query.as_str(), gold.kind), ("@GC.1", Kind::Future));
    }

    #[test]
    fn share_classes_use_a_dot() {
        assert_eq!(one("BRK-B").query, "BRK.B");
        assert_eq!(one("brk/b").query, "BRK.B");
        assert_eq!(one("BRK.B").query, "BRK.B");
        assert_eq!(one("BF-B").label, "BF-B");
    }

    #[test]
    fn a_name_it_cannot_place_is_kept_as_typed() {
        let e = one("appl");
        assert_eq!((e.label.as_str(), e.query.as_str()), ("APPL", "APPL"));
    }

    #[test]
    fn the_list_drops_repeats_and_stops_at_the_limit() {
        let list = resolve_all(&["NVDA", "nvda", "BTC", "BTC-USD", "", "TSLA"]);
        let queries: Vec<&str> = list.iter().map(|e| e.query.as_str()).collect();
        assert_eq!(queries, ["NVDA", "BTC.CM=", "TSLA"]);

        let many: Vec<String> = (0..30).map(|i| fmt_letters(i)).collect();
        assert_eq!(resolve_all(&many).len(), MAX_SYMBOLS);
    }

    fn fmt_letters(i: u8) -> String {
        let mut s = String::from("AB");
        s.push(char::from(b'A' + i % 26));
        s.push(char::from(b'A' + i / 26));
        s
    }

    #[test]
    fn plain_tickers_try_the_main_market_and_then_the_builders() {
        assert_eq!(one("nvda").hl, ["NVDA", "xyz:NVDA"]);
        assert_eq!(one("BRK.B").hl, Vec::<String>::new(), "a name with a dot is not a coin there");
    }

    #[test]
    fn coins_are_perpetuals_of_the_main_market() {
        assert_eq!(one("BTC-USD").hl, ["BTC"]);
        assert_eq!(one("sol-usd").hl, ["SOL", "kSOL"]);
    }

    #[test]
    fn what_has_a_builder_market_names_it() {
        assert_eq!(one("gold").hl, ["xyz:GOLD"]);
        assert_eq!(one("^GSPC").hl, ["xyz:SP500"]);
        assert_eq!(one("vix").hl, ["xyz:VIX"]);
        assert!(one("^DJI").hl.is_empty());
    }

    #[test]
    fn a_coin_written_with_its_market_is_taken_as_written() {
        let e = one("xyz:nvda");
        assert_eq!((e.label.as_str(), e.query.as_str(), e.hl.as_slice()), ("NVDA", "NVDA", ["xyz:NVDA".to_string()].as_slice()));
        assert_eq!(one("XYZ:GOLD").hl, ["xyz:GOLD"]);
        assert_eq!(one("km:us500").hl, ["km:US500"]);
    }

    #[test]
    fn a_builder_coin_for_a_commodity_or_an_index_is_that_on_cnbc_too() {
        let gold = one("xyz:gold");
        assert_eq!((gold.query.as_str(), gold.label.as_str(), gold.kind), ("@GC.1", "GOLD", Kind::Future));
        assert_eq!(gold.hl, ["xyz:GOLD"]);
        let oil = one("xyz:cl");
        assert_eq!((oil.query.as_str(), oil.label.as_str()), ("@CL.1", "OIL"));
        assert_eq!(one("xyz:brentoil").query, "@LCO.1");
        assert_eq!(one("xyz:sp500").query, ".SPX");
        assert_eq!(one("xyz:xyz100").query, ".NDX");
        // The same instrument typed two ways is one row.
        assert_eq!(resolve_all(&["xyz:GOLD", "XAU-USD", "gold"]).len(), 1);
        // A stock is still its ticker, and a plain CL is still Colgate-Palmolive.
        assert_eq!(one("xyz:nvda").query, "NVDA");
        assert_eq!((one("CL").query.as_str(), one("CL").kind), ("CL", Kind::Stock));
    }

    #[test]
    fn the_cnbc_prefix_asks_for_that_service_alone() {
        let e = one("cnbc:spy");
        assert_eq!((e.query.as_str(), e.kind), ("SPY", Kind::Stock));
        assert!(e.hl.is_empty());
        let coin = one("cnbc:btc-usd");
        assert_eq!(coin.query, "BTC.CM=");
        assert!(coin.hl.is_empty());
    }

    #[test]
    fn the_source_setting_defaults_to_hyperliquid() {
        assert_eq!(Source::from_param(None), Source::Hyperliquid);
        assert_eq!(Source::from_param(Some("hyperliquid")), Source::Hyperliquid);
        assert_eq!(Source::from_param(Some("something odd")), Source::Hyperliquid);
        assert_eq!(Source::from_param(Some("cnbc")), Source::Cnbc);
        assert_eq!(Source::from_param(Some(" CNBC ")), Source::Cnbc);
    }

    #[test]
    fn the_request_encodes_what_a_url_reserves() {
        let list = resolve_all(&["NVDA", "BTC-USD", "^GSPC", "XAU-USD"]);
        assert_eq!(request_list(&list), "NVDA%7CBTC.CM%3D%7C.SPX%7C%40GC.1");
    }
}
