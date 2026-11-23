//! The watchlist: what the operator typed, and what CNBC's quote service calls it.

/// The most symbols a list can hold. The manifest says the same.
pub const MAX_SYMBOLS: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Stock,
    Index,
    Crypto,
    Future,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// What the screen calls it: `NVDA`, `BTC`, `S&P 500`.
    pub label: String,
    /// Its symbol in the quote service: `NVDA`, `BTC.CM=`, `.SPX`.
    pub query: String,
    pub kind: Kind,
}

/// The names people type for what the quote service writes differently.
fn alias(typed: &str) -> Option<(&'static str, &'static str, Kind)> {
    Some(match typed {
        "^GSPC" | "GSPC" | "SPX" | ".SPX" | "S&P500" | "S&P" | "SP500" => (".SPX", "S&P 500", Kind::Index),
        "^IXIC" | "IXIC" | ".IXIC" | "NASDAQ" | "COMP" | "NASDAQCOMPOSITE" => (".IXIC", "NASDAQ", Kind::Index),
        "^DJI" | "DJI" | ".DJI" | "DOW" | "DJIA" => (".DJI", "DOW", Kind::Index),
        "^NDX" | "NDX" | ".NDX" => (".NDX", "NASDAQ 100", Kind::Index),
        "^RUT" | "RUT" | ".RUT" => (".RUT", "RUSSELL 2000", Kind::Index),
        "GOLD" | "XAU" | "XAU-USD" | "XAUUSD" | "GC=F" => ("@GC.1", "GOLD", Kind::Future),
        "BITCOIN" | "BTC" | "BTC-USD" | "BTCUSD" | "XBT" => ("BTC.CM=", "BTC", Kind::Crypto),
        "ETHEREUM" | "ETH" | "ETH-USD" | "ETHUSD" => ("ETH.CM=", "ETH", Kind::Crypto),
        _ => return None,
    })
}

/// `SOL-USD` and the like: a coin against the dollar.
fn crypto_pair(typed: &str) -> Option<String> {
    let base = typed.strip_suffix("-USD")?;
    let fits = (2..=6).contains(&base.len()) && base.bytes().all(|b| b.is_ascii_alphanumeric());
    fits.then(|| base.to_string())
}

/// One list item as the quote service knows it; `None` for a blank item.
///
/// A name it cannot place is kept as typed, so that the screen says "No data" for it
/// and a typo shows instead of vanishing.
pub fn resolve(raw: &str) -> Option<Entry> {
    let typed: String = raw.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_uppercase();
    if typed.is_empty() {
        return None;
    }
    if let Some((query, label, kind)) = alias(&typed) {
        return Some(Entry { label: label.to_string(), query: query.to_string(), kind });
    }
    if let Some(base) = crypto_pair(&typed) {
        return Some(Entry { query: fmt_coin(&base), label: base, kind: Kind::Crypto });
    }
    // Share classes: the service writes BRK.B where people write BRK-B or BRK/B.
    let query = share_class(&typed);
    Some(Entry { label: typed, query, kind: Kind::Stock })
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
    fn the_request_encodes_what_a_url_reserves() {
        let list = resolve_all(&["NVDA", "BTC-USD", "^GSPC", "XAU-USD"]);
        assert_eq!(request_list(&list), "NVDA%7CBTC.CM%3D%7C.SPX%7C%40GC.1");
    }
}
