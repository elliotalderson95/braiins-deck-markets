//! A quote as CNBC's service reports it, and what the screen makes of it.
//!
//! The service writes every number as text ("7,244.66", "+1.81%") and reports one regular
//! quote plus one extended-hours quote: the pre-market trade, until the market opens, or
//! the after-hours trade, once it has closed. This module turns that into plain values and
//! decides which price and which move a tile shows in each session.

/// The quote service's fields for one symbol, as text. The widget glue fills it from the reply.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Raw {
    pub symbol: String,
    /// 0 when the service knows the symbol.
    pub code: i64,
    pub name: String,
    pub last: String,
    pub prev_close: String,
    pub change_pct: String,
    /// `REG_MKT`, `PRE_MKT`, `POST_MKT` or `CLOSED`.
    pub status: String,
    pub real_time: String,
    pub last_time: String,
    /// `PRE_MKT` or `POST_MKT`; empty without an extended-hours quote.
    pub ext_type: String,
    pub ext_last: String,
    pub ext_change_pct: String,
    pub ext_time: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Session {
    Pre,
    Regular,
    Post,
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtKind {
    Pre,
    Post,
}

/// The latest trade outside regular hours.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ext {
    pub kind: ExtKind,
    pub price: f64,
    /// Against the previous close for pre-market, against the close for after-hours.
    pub pct: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Quote {
    pub name: String,
    pub session: Session,
    /// The last regular-session price.
    pub last: f64,
    pub prev_close: Option<f64>,
    /// `last` against the previous close, in percent.
    pub day_pct: Option<f64>,
    pub ext: Option<Ext>,
    pub real_time: bool,
    /// Unix time of the last trade; 0 when the service gave none.
    pub traded_at: i64,
    /// From Hyperliquid: a perpetual that trades round the clock. Its "previous close" is the price a day ago.
    pub perp: bool,
    /// The dollars traded in the last day, when the source says.
    pub volume: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tag {
    Pre,
    After,
    Closed,
    Delayed,
    Perp,
}

/// The line under the price.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Note {
    None,
    /// How the stock traded before the open.
    PreMarket(Option<f64>),
    PrevClose(f64),
    /// The regular close and its move, under an after-hours price.
    Close { price: f64, pct: Option<f64> },
    NoPreMarketTrades,
    /// The dollars traded in the last day.
    Volume(f64),
}

/// What a tile shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub price: f64,
    /// The big colored move; `None` when there is none to show.
    pub pct: Option<f64>,
    pub tag: Option<Tag>,
    pub note: Note,
}

impl Quote {
    /// The tile for this quote in its session.
    ///
    /// Regular hours show the last price and the move of the day, with how the pre-market went
    /// under it. Before the open the pre-market trade leads; after the close the after-hours
    /// trade leads and the day's own close and move sit under it.
    #[must_use]
    pub fn view(&self) -> View {
        if self.perp {
            return View {
                price: self.last,
                pct: self.day_pct,
                tag: Some(Tag::Perp),
                note: self.volume.map_or_else(|| self.prev_close.map_or(Note::None, Note::PrevClose), Note::Volume),
            };
        }
        let delayed = if self.real_time { None } else { Some(Tag::Delayed) };
        match self.session {
            Session::Regular => View {
                price: self.last,
                pct: self.day_pct,
                tag: delayed,
                note: match self.ext {
                    Some(Ext { kind: ExtKind::Pre, price, pct }) => Note::PreMarket(pre_market_pct(price, self.prev_close, pct)),
                    _ => self.prev_close.map_or(Note::None, Note::PrevClose),
                },
            },
            Session::Pre => match self.ext {
                Some(Ext { kind: ExtKind::Pre, price, pct }) => View {
                    price,
                    pct: pre_market_pct(price, self.prev_close, pct),
                    tag: Some(Tag::Pre),
                    note: self.prev_close.map_or(Note::None, Note::PrevClose),
                },
                _ => View {
                    price: self.last,
                    pct: None,
                    tag: Some(Tag::Pre),
                    note: Note::NoPreMarketTrades,
                },
            },
            Session::Post | Session::Closed => {
                let tag = if self.session == Session::Post { Tag::After } else { Tag::Closed };
                match self.ext {
                    Some(Ext { kind: ExtKind::Post, price, pct }) => View {
                        price,
                        pct: after_hours_pct(price, self.last, pct),
                        tag: Some(tag),
                        note: Note::Close { price: self.last, pct: self.day_pct },
                    },
                    _ => View {
                        price: self.last,
                        pct: self.day_pct,
                        tag: delayed.or(Some(tag)),
                        note: self.prev_close.map_or(Note::None, Note::PrevClose),
                    },
                }
            }
        }
    }

    /// Reads the service's text fields; `None` when the symbol is unknown or has no price.
    #[must_use]
    pub fn from_raw(raw: &Raw) -> Option<Self> {
        if raw.code != 0 {
            return None;
        }
        let last = number(&raw.last)?;
        let prev_close = number(&raw.prev_close);
        let day_pct = number(&raw.change_pct).or_else(|| against(last, prev_close));
        let session = match raw.status.as_str() {
            "PRE_MKT" => Session::Pre,
            "POST_MKT" => Session::Post,
            "CLOSED" => Session::Closed,
            _ => Session::Regular,
        };
        let kind = match raw.ext_type.as_str() {
            "PRE_MKT" => Some(ExtKind::Pre),
            "POST_MKT" => Some(ExtKind::Post),
            _ => None,
        };
        let ext = kind.zip(number(&raw.ext_last)).map(|(kind, price)| Ext {
            kind,
            price,
            pct: number(&raw.ext_change_pct),
        });
        Some(Self {
            name: raw.name.trim().to_string(),
            session,
            last,
            prev_close,
            day_pct,
            ext,
            real_time: !raw.real_time.eq_ignore_ascii_case("false"),
            traded_at: unix_time(&raw.last_time).unwrap_or(0),
            perp: false,
            volume: None,
        })
    }

    /// A Hyperliquid market's context as a quote: its mark price, against the price a day ago.
    /// `None` when it carries no price.
    #[must_use]
    pub fn from_hl(coin: &str, ctx: &crate::hl::Ctx, now: i64) -> Option<Self> {
        let last = number(&ctx.mark).or_else(|| number(&ctx.mid)).filter(|p| *p > 0.0)?;
        let prev_close = number(&ctx.prev_day).filter(|p| *p > 0.0);
        let name = if coin.contains(':') { coin.to_string() } else { [coin, " perpetual"].concat() };
        Some(Self {
            name,
            session: Session::Regular,
            last,
            prev_close,
            day_pct: against(last, prev_close),
            ext: None,
            real_time: true,
            traded_at: now,
            perp: true,
            volume: number(&ctx.volume).filter(|v| *v > 0.0),
        })
    }
}

/// The pre-market move: the pre-market price against the previous close. The service's own
/// figure is the fallback when the close is not known.
fn pre_market_pct(price: f64, prev_close: Option<f64>, reported: Option<f64>) -> Option<f64> {
    against(price, prev_close).or(reported)
}

/// The after-hours move: the after-hours price against the regular close. If the service has
/// already folded the after-hours price into `last`, there is no close to compare with, and its
/// own figure is all there is.
fn after_hours_pct(price: f64, close: f64, reported: Option<f64>) -> Option<f64> {
    if (price - close).abs() < f64::EPSILON {
        reported
    } else {
        against(price, Some(close))
    }
}

/// `price` against `base`, in percent.
fn against(price: f64, base: Option<f64>) -> Option<f64> {
    base.filter(|b| *b != 0.0).map(|b| (price / b - 1.0) * 100.0)
}

/// A number the service wrote as text: "7,244.66", "+6.19", "-1.81%", "UNCH". `None` for "N/A" and blanks.
#[must_use]
pub fn number(text: &str) -> Option<f64> {
    let t: String = text.chars().filter(|c| !matches!(c, ',' | '+' | '%' | '$') && !c.is_whitespace()).collect();
    if t.eq_ignore_ascii_case("UNCH") {
        return Some(0.0);
    }
    t.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Unix seconds from "2026-11-23T11:03:39.723-0500".
#[must_use]
pub fn unix_time(text: &str) -> Option<i64> {
    let digits = |from: usize, to: usize| -> Option<i64> { text.get(from..to)?.parse::<i64>().ok() };
    let (year, month, day) = (digits(0, 4)?, digits(5, 7)?, digits(8, 10)?);
    let (hour, minute, second) = (digits(11, 13)?, digits(14, 16)?, digits(17, 19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let zone = text.get(19..)?;
    let offset = match zone.find(['+', '-']) {
        Some(i) => {
            let z = zone.get(i..)?;
            let sign = if z.starts_with('-') { -1 } else { 1 };
            let hh = z.get(1..3)?.parse::<i64>().ok()?;
            let mm = z.get(3..5)?.parse::<i64>().ok()?;
            sign * (hh * 3600 + mm * 60)
        }
        None => 0,
    };
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

/// Days since 1970-01-01 of a calendar date (Howard Hinnant's algorithm).
pub(crate) fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(status: &str) -> Raw {
        Raw {
            symbol: "AAPL".into(),
            code: 0,
            name: "Apple Inc.".into(),
            last: "348.75".into(),
            prev_close: "342.56".into(),
            change_pct: "+1.81%".into(),
            status: status.into(),
            real_time: "true".into(),
            last_time: "2026-11-23T11:03:39.723-0500".into(),
            ext_type: "PRE_MKT".into(),
            ext_last: "347.82".into(),
            ext_change_pct: "+1.54%".into(),
            ext_time: "2026-11-23T09:29:59.980-0500".into(),
        }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn numbers_are_read_through_the_services_formatting() {
        assert_eq!(number("7,244.66"), Some(7244.66));
        assert_eq!(number("+6.19"), Some(6.19));
        assert_eq!(number("-1.81%"), Some(-1.81));
        assert_eq!(number("UNCH"), Some(0.0));
        assert_eq!(number("N/A"), None);
        assert_eq!(number(""), None);
        assert_eq!(number("24,184.801"), Some(24184.801));
    }

    #[test]
    fn times_are_read_with_their_offset() {
        // 2026-11-23 11:03:39 at UTC-5 is 16:03:39 UTC.
        assert_eq!(unix_time("2026-11-23T11:03:39.723-0500"), Some(1_795_449_819));
        assert_eq!(unix_time("2026-11-23T16:03:39Z"), Some(1_795_449_819));
        assert_eq!(unix_time("1970-01-01T00:00:00+0000"), Some(0));
        assert_eq!(unix_time("2026-11-23T17:03:39+0100"), Some(1_795_449_819));
        assert_eq!(unix_time("garbage"), None);
        assert_eq!(unix_time("2026-13-01T00:00:00+0000"), None);
    }

    #[test]
    fn an_unknown_symbol_has_no_quote() {
        let mut r = raw("REG_MKT");
        r.code = 1;
        assert_eq!(Quote::from_raw(&r), None);
        let mut r = raw("REG_MKT");
        r.last = String::new();
        assert_eq!(Quote::from_raw(&r), None);
    }

    #[test]
    fn regular_hours_lead_with_the_day_and_recall_the_pre_market() {
        let q = Quote::from_raw(&raw("REG_MKT")).expect("BUG: a good sample");
        assert_eq!(q.session, Session::Regular);
        let v = q.view();
        assert!(close(v.price, 348.75));
        assert_eq!(v.pct, Some(1.81));
        assert_eq!(v.tag, None);
        let Note::PreMarket(Some(pre)) = v.note else { panic!("BUG: expected the pre-market recap, got {:?}", v.note) };
        assert!(close(pre, (347.82 / 342.56 - 1.0) * 100.0));
    }

    #[test]
    fn before_the_open_the_pre_market_trade_leads() {
        let q = Quote::from_raw(&raw("PRE_MKT")).expect("BUG: a good sample");
        let v = q.view();
        assert!(close(v.price, 347.82));
        assert!(close(v.pct.expect("BUG: a pre-market move"), (347.82 / 342.56 - 1.0) * 100.0));
        assert_eq!(v.tag, Some(Tag::Pre));
        assert_eq!(v.note, Note::PrevClose(342.56));
    }

    #[test]
    fn before_the_open_a_stale_after_hours_quote_is_ignored() {
        let mut r = raw("PRE_MKT");
        r.ext_type = "POST_MKT".into();
        let v = Quote::from_raw(&r).expect("BUG: a good sample").view();
        assert!(close(v.price, 348.75));
        assert_eq!(v.pct, None);
        assert_eq!(v.note, Note::NoPreMarketTrades);
    }

    #[test]
    fn after_hours_lead_with_the_extended_trade_and_keep_the_close_beside_it() {
        let mut r = raw("POST_MKT");
        r.ext_type = "POST_MKT".into();
        r.ext_last = "349.50".into();
        r.ext_change_pct = "+0.21%".into();
        let v = Quote::from_raw(&r).expect("BUG: a good sample").view();
        assert!(close(v.price, 349.50));
        assert!(close(v.pct.expect("BUG: an after-hours move"), (349.50 / 348.75 - 1.0) * 100.0));
        assert_eq!(v.tag, Some(Tag::After));
        assert_eq!(v.note, Note::Close { price: 348.75, pct: Some(1.81) });
    }

    #[test]
    fn a_missing_extended_move_is_worked_out_from_the_close() {
        let mut r = raw("POST_MKT");
        r.ext_type = "POST_MKT".into();
        r.ext_last = "350.00".into();
        r.ext_change_pct = String::new();
        let v = Quote::from_raw(&r).expect("BUG: a good sample").view();
        let pct = v.pct.expect("BUG: worked out from the close");
        assert!((pct - (350.0 / 348.75 - 1.0) * 100.0).abs() < 1e-9);
    }

    #[test]
    fn the_after_hours_move_is_against_the_close_whatever_baseline_the_service_used() {
        let mut r = raw("POST_MKT");
        r.ext_type = "POST_MKT".into();
        r.ext_last = "350.00".into();
        // The service's figure here would be against the previous close.
        r.ext_change_pct = "+2.17%".into();
        let pct = Quote::from_raw(&r).expect("BUG: a good sample").view().pct.expect("BUG: a move");
        assert!(close(pct, (350.0 / 348.75 - 1.0) * 100.0));
    }

    #[test]
    fn the_services_own_figure_stands_when_it_has_folded_the_extended_price_into_the_close() {
        let mut r = raw("POST_MKT");
        r.ext_type = "POST_MKT".into();
        r.last = "350.00".into();
        r.ext_last = "350.00".into();
        r.ext_change_pct = "+0.36%".into();
        assert_eq!(Quote::from_raw(&r).expect("BUG: a good sample").view().pct, Some(0.36));
    }

    #[test]
    fn closed_without_after_hours_trades_shows_the_close() {
        let mut r = raw("CLOSED");
        r.ext_type = String::new();
        r.ext_last = String::new();
        let v = Quote::from_raw(&r).expect("BUG: a good sample").view();
        assert!(close(v.price, 348.75));
        assert_eq!(v.pct, Some(1.81));
        assert_eq!(v.tag, Some(Tag::Closed));
    }

    #[test]
    fn delayed_quotes_are_marked() {
        let mut r = raw("REG_MKT");
        r.real_time = "false".into();
        assert_eq!(Quote::from_raw(&r).expect("BUG: a good sample").view().tag, Some(Tag::Delayed));
    }

    #[test]
    fn the_day_move_is_worked_out_when_the_service_leaves_it_out() {
        let mut r = raw("REG_MKT");
        r.change_pct = String::new();
        let pct = Quote::from_raw(&r).expect("BUG: a good sample").day_pct.expect("BUG: worked out");
        assert!((pct - (348.75 / 342.56 - 1.0) * 100.0).abs() < 1e-9);
    }

    fn ctx(mark: &str, prev: &str, volume: &str) -> crate::hl::Ctx {
        crate::hl::Ctx { mark: mark.into(), mid: String::new(), prev_day: prev.into(), volume: volume.into() }
    }

    #[test]
    fn a_perpetual_shows_its_price_its_move_over_a_day_and_its_volume() {
        let q = Quote::from_hl("xyz:NVDA", &ctx("163.43", "134.51", "1615050662.07"), 1_000).expect("BUG: a good context");
        assert_eq!(q.session, Session::Regular);
        assert_eq!(q.name, "xyz:NVDA");
        let v = q.view();
        assert!(close(v.price, 163.43));
        assert!(close(v.pct.expect("BUG: a move"), (163.43 / 134.51 - 1.0) * 100.0));
        assert_eq!(v.tag, Some(Tag::Perp));
        assert_eq!(v.note, Note::Volume(1_615_050_662.07));
        assert_eq!(Quote::from_hl("BTC", &ctx("60317.0", "61521.0", "4817910048.4"), 1).expect("BUG: a good context").name, "BTC perpetual");
    }

    #[test]
    fn a_context_without_a_price_is_no_quote_and_the_mid_stands_in_for_a_missing_mark() {
        assert_eq!(Quote::from_hl("BTC", &ctx("", "100", "1"), 1), None);
        assert_eq!(Quote::from_hl("BTC", &ctx("0", "100", "1"), 1), None);
        let mut c = ctx("", "100", "");
        c.mid = "101.5".into();
        let q = Quote::from_hl("BTC", &c, 1).expect("BUG: the mid is a price");
        assert!(close(q.last, 101.5));
        assert_eq!(q.view().note, Note::PrevClose(100.0), "no volume, so the old price is shown");
    }
}
