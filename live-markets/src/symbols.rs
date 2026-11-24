//! The watchlist: what the operator typed, what CNBC's quote service calls it, what Hyperliquid does, and
//! what TradingView calls the economic indicators and indexes that only it has.

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
    /// An economic indicator from TradingView (the ISM PMI, inflation, jobs): a value released now and then.
    Indicator,
}

/// Where a symbol's prices come from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Source {
    #[default]
    Hyperliquid,
    Cnbc,
    /// TradingView, which prices the economic indicators and the indexes that the others lack. It is never the
    /// operator's choice for the list: a symbol is its own when it has no other name.
    Tv,
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

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// What the screen calls it: `NVDA`, `BTC`, `S&P 500`.
    pub label: String,
    /// Its symbol in the quote service: `NVDA`, `BTC.CM=`, `.SPX`.
    pub query: String,
    /// What it is on CNBC; a Hyperliquid perpetual is `Kind::Perp` whatever this says.
    pub kind: Kind,
    /// Its names on Hyperliquid, best first: `BTC`, `xyz:NVDA`. None for what only CNBC has.
    pub hl: Vec<String>,
    /// Its ticker on TradingView (`ECONOMICS:USBCOI`), for what only that has; such a symbol is priced there.
    pub tv: Option<String>,
    /// The line that matters in its chart, such as the 50 that divides growth from contraction in a PMI.
    pub pivot: Option<f64>,
    /// What to call it under its label, when the service's own name is not the one wanted.
    pub name: Option<String>,
}

/// An entry with only the names the market services know it by.
fn market(label: String, query: String, kind: Kind, hl: Vec<String>) -> Entry {
    Entry { label, query, kind, hl, tv: None, pivot: None, name: None }
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

/// The builder markets of Hyperliquid, which write their coins `dex:NAME` with the market in lower case.
const HL_DEXES: [&str; 11] = ["xyz", "flx", "vntl", "hyna", "km", "cash", "para", "mkts", "io", "abcd", "hl"];

/// `xyz:NVDA`, `cnbc:NVDA`, `ECONOMICS:USBCOI`: a prefix, the name of a market or of a service, and what
/// that market calls the symbol. Any symbol of CNBC's goes after `cnbc:`, and a whole TradingView ticker
/// after `tv:`, so the tail may hold a hyphen, a slash or one more colon.
fn prefixed(raw: &str) -> Option<(&str, &str)> {
    let (head, tail) = raw.split_once(':')?;
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'.';
    let head_fits = !head.is_empty() && head.len() <= 16 && head.bytes().all(word);
    let tail_fits = !tail.is_empty() && tail.len() <= 32 && tail.bytes().all(|b| word(b) || matches!(b, b'-' | b'/' | b'^' | b'=' | b'&' | b'!' | b':'));
    (head_fits && tail_fits).then_some((head, tail))
}

/// A symbol of TradingView's from its ticker, `EXCHANGE:NAME`. What is not an economic indicator is shown
/// like a future: priced round the clock by the exchange's clock, with no sessions drawn.
fn tv_entry(ticker: &str) -> Option<Entry> {
    let (exchange, name) = ticker.split_once(':')?;
    if exchange.is_empty() || name.is_empty() {
        return None;
    }
    let kind = if exchange == "ECONOMICS" { Kind::Indicator } else { Kind::Future };
    Some(Entry { label: name.to_string(), query: ticker.to_string(), kind, hl: Vec::new(), tv: Some(ticker.to_string()), pivot: None, name: None })
}

/// A TradingView symbol the way a name is given for it: its ticker, label, kind, pivot line and display name.
type TvAlias = (&'static str, &'static str, Kind, Option<f64>, Option<&'static str>);

/// The names people type for what TradingView has: its ticker, the label, the kind, the line that matters in
/// its chart, and the name to show. `key` is the typed name in capitals without hyphens or underscores.
fn tv_alias(key: &str) -> Option<TvAlias> {
    let economic = |ticker, label| Some((ticker, label, Kind::Indicator, None, None));
    // A purchasing managers index: above 50 the industry grows, below it shrinks.
    let pmi = |ticker, label| Some((ticker, label, Kind::Indicator, Some(50.0), None));
    // What trades on a futures or a bond market, with the name to show under its label.
    let market = |ticker, label, name| Some((ticker, label, Kind::Future, None, Some(name)));
    match key {
        "US10Y" | "US10YR" => market("TVC:US10Y", "US10Y", "US 10 Year Treasury Yield"),
        "US2Y" | "US02Y" => market("TVC:US02Y", "US2Y", "US 2 Year Treasury Yield"),
        "US5Y" | "US05Y" => market("TVC:US05Y", "US5Y", "US 5 Year Treasury Yield"),
        "US30Y" => market("TVC:US30Y", "US30Y", "US 30 Year Treasury Yield"),
        "DXY" | "USDX" => market("TVC:DXY", "DXY", "US Dollar Index (DXY)"),
        "COPPER" => market("COMEX:HG1!", "COPPER", "Copper futures"),
        "NATGAS" | "NATURALGAS" => market("NYMEX:NG1!", "NATGAS", "Natural gas futures"),
        "PLATINUM" => market("NYMEX:PL1!", "PLATINUM", "Platinum futures"),
        "PALLADIUM" => market("NYMEX:PA1!", "PALLADIUM", "Palladium futures"),
        "ISM" | "ISMPMI" | "PMI" | "ISMMANUFACTURING" | "ISMMANUFACTURINGPMI" => pmi("ECONOMICS:USBCOI", "ISM PMI"),
        "ISMNEWORDERS" => pmi("ECONOMICS:USMNO", "ISM ORD"),
        "ISMPRICES" => pmi("ECONOMICS:USMPR", "ISM PRC"),
        "ISMEMPLOYMENT" => pmi("ECONOMICS:USMEMP", "ISM JOB"),
        "ISMSERVICES" | "ISMNONMANUFACTURING" => pmi("ECONOMICS:USNMBA", "ISM SVC"),
        "CPI" | "INFLATION" => economic("ECONOMICS:USIRYY", "CPI YOY"),
        "CORECPI" | "COREINFLATION" => economic("ECONOMICS:USCIR", "CORE"),
        "CPIINDEX" => economic("ECONOMICS:USCPI", "CPI IDX"),
        "PPI" => economic("ECONOMICS:USPPIYY", "PPI YOY"),
        "UNEMPLOYMENT" | "UNRATE" => economic("ECONOMICS:USUR", "UNRATE"),
        "NFP" | "PAYROLLS" | "NONFARM" | "NONFARMPAYROLLS" => economic("ECONOMICS:USNFP", "NFP"),
        "GDP" | "GDPGROWTH" => economic("ECONOMICS:USGDPQQ", "GDP QOQ"),
        "FEDRATE" | "FEDFUNDS" | "FOMC" | "INTERESTRATE" => economic("ECONOMICS:USINTR", "FED"),
        "RETAILSALES" => economic("ECONOMICS:USRSMM", "RETAIL"),
        "CLAIMS" | "JOBLESSCLAIMS" | "INITIALCLAIMS" => economic("ECONOMICS:USIJC", "CLAIMS"),
        "CONSUMERCONFIDENCE" => economic("ECONOMICS:USCCI", "CONF"),
        "HOUSINGSTARTS" => economic("ECONOMICS:USHST", "STARTS"),
        "INDUSTRIALPRODUCTION" => economic("ECONOMICS:USIPMM", "IND PROD"),
        "TRADEBALANCE" => economic("ECONOMICS:USBOT", "TRADE"),
        // The index itself, in real time; CBOE:VIX is the same index 15 minutes late.
        "VIX" | "^VIX" | ".VIX" | "CBOEVIX" => Some(("TVC:VIX", "VIX", Kind::Index, None, Some("Cboe Volatility Index (VIX)"))),
        _ => None,
    }
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
        let (name, head) = (tail.to_uppercase(), head.to_lowercase());
        if head == "cnbc" {
            let mut entry = resolve_cnbc(&name)?;
            entry.hl.clear();
            return Some(entry);
        }
        if head == "tv" || head == "tradingview" {
            return tv_entry(&name);
        }
        // A coin exactly as Hyperliquid names it. CNBC has the commodities and indexes under other names, and
        // for the rest the plain ticker is its best guess.
        if HL_DEXES.contains(&head.as_str()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.')) {
            let coin = format!("{head}:{name}");
            if let Some((query, label, kind, _)) = alias(builder_name(&name)) {
                return Some(market(label.to_string(), query.to_string(), kind, vec![coin]));
            }
            return Some(market(name.clone(), name, Kind::Stock, vec![coin]));
        }
        // Any other prefix is an exchange of TradingView's: ECONOMICS:USBCOI, TVC:DXY, NASDAQ:NVDA.
        let exchange = if head == "econ" { "ECONOMICS".to_string() } else { head.to_uppercase() };
        return tv_entry(&format!("{exchange}:{name}"));
    }
    let typed = compact.to_uppercase();
    let key: String = typed.chars().filter(|c| !matches!(c, '-' | '_')).collect();
    if let Some((ticker, label, kind, pivot, name)) = tv_alias(&key) {
        let mut entry = tv_entry(ticker)?;
        (entry.label, entry.kind, entry.pivot, entry.name) = (label.to_string(), kind, pivot, name.map(str::to_string));
        return Some(entry);
    }
    let mut entry = resolve_cnbc(&typed)?;
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
        return Some(market(label.to_string(), query.to_string(), kind, hl.iter().map(|c| (*c).to_string()).collect()));
    }
    if let Some(base) = crypto_pair(typed) {
        // Hyperliquid writes the small coins of the main market with a k: kPEPE.
        let hl = vec![base.clone(), format!("k{base}")];
        return Some(market(base.clone(), fmt_coin(&base), Kind::Crypto, hl));
    }
    // Share classes: the service writes BRK.B where people write BRK-B or BRK/B.
    let query = share_class(typed);
    Some(market(typed.to_string(), query, Kind::Stock, Vec::new()))
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
        assert_eq!(one("xyz:vix").hl, ["xyz:VIX"], "the perpetual on the VIX is named, not guessed");
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
    fn the_ism_purchasing_managers_index_is_a_tradingview_indicator_with_its_fifty_line() {
        for typed in ["ISM", "ism-pmi", "PMI", "ISM PMI", "ism_manufacturing"] {
            let e = one(typed);
            assert_eq!((e.tv.as_deref(), e.label.as_str(), e.kind, e.pivot), (Some("ECONOMICS:USBCOI"), "ISM PMI", Kind::Indicator, Some(50.0)), "{typed}");
            assert_eq!(e.query, "ECONOMICS:USBCOI");
            assert!(e.hl.is_empty());
        }
        let jobs = one("unemployment");
        assert_eq!((jobs.tv.as_deref(), jobs.pivot), (Some("ECONOMICS:USUR"), None));
        assert_eq!(one("Fed-Rate").tv.as_deref(), Some("ECONOMICS:USINTR"));
        assert_eq!(one("nfp").tv.as_deref(), Some("ECONOMICS:USNFP"));
    }

    #[test]
    fn any_economic_ticker_can_be_written_as_tradingview_writes_it() {
        let e = one("economics:usbcoi");
        assert_eq!((e.tv.as_deref(), e.label.as_str(), e.kind, e.pivot), (Some("ECONOMICS:USBCOI"), "USBCOI", Kind::Indicator, None));
        assert_eq!(one("econ:USCCI").tv.as_deref(), Some("ECONOMICS:USCCI"));
        assert_eq!(one("tv:economics:usnfp").tv.as_deref(), Some("ECONOMICS:USNFP"));
    }

    #[test]
    fn the_vix_is_the_index_in_real_time_and_other_tickers_follow_tradingviews_names() {
        for typed in ["VIX", "^VIX", ".vix"] {
            let e = one(typed);
            assert_eq!((e.tv.as_deref(), e.label.as_str(), e.kind), (Some("TVC:VIX"), "VIX", Kind::Index), "{typed}");
            assert_eq!(e.name.as_deref(), Some("Cboe Volatility Index (VIX)"));
        }
        let delayed = one("cboe:vix");
        assert_eq!((delayed.tv.as_deref(), delayed.label.as_str()), (Some("CBOE:VIX"), "VIX"));
        assert_eq!(one("tv:TVC:DXY").tv.as_deref(), Some("TVC:DXY"));
        assert_eq!(one("nasdaq:nvda").tv.as_deref(), Some("NASDAQ:NVDA"));
        // The Hyperliquid perpetual on the VIX and CNBC's own VIX are asked for by name.
        assert_eq!(one("xyz:VIX").tv, None);
        assert_eq!(one("cnbc:VIX").tv, None);
        assert_eq!(resolve_all(&["VIX", "tvc:vix", "^VIX"]).len(), 1, "the same ticker is one row");
    }

    #[test]
    fn yields_the_dollar_and_metals_without_a_hyperliquid_market_come_from_tradingview() {
        let ten = one("us10y");
        assert_eq!((ten.tv.as_deref(), ten.label.as_str(), ten.kind), (Some("TVC:US10Y"), "US10Y", Kind::Future));
        assert_eq!(ten.name.as_deref(), Some("US 10 Year Treasury Yield"));
        assert_eq!(one("US-02Y").tv.as_deref(), Some("TVC:US02Y"));
        assert_eq!(one("dxy").tv.as_deref(), Some("TVC:DXY"));
        assert_eq!(one("copper").tv.as_deref(), Some("COMEX:HG1!"));
        assert_eq!(one("comex:hg1!").tv.as_deref(), Some("COMEX:HG1!"), "typed as TradingView writes it");
        assert_eq!(one("xyz:DXY").tv, None, "the Hyperliquid market of the dollar index is named, not guessed");
        assert_eq!(one("gold").tv, None, "gold keeps its Hyperliquid market");
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
