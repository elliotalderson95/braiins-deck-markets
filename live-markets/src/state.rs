//! What the widget knows: the watchlist, the latest quote of each symbol, and how fresh they are.

use crate::chart::{Frame, Series};
use crate::clock;
use crate::hl::{self, Ctx};
use crate::quote::{Quote, Raw, Session};
use crate::symbols::{Entry, Kind, Source, request_list};
use crate::view::{Hits, Tap, resolve};

/// How long a price flashes green or red after it ticks.
pub const FLASH_MS: u64 = 900;
/// How long a chart stays up after the last tap before the list comes back.
pub const DETAIL_IDLE_MS: u64 = 90_000;
/// The cursor's speed under a held arrow, in widths of the chart per second: still for a moment, so
/// that a tap is one notch, then slow and soon fast.
fn jog_speed(held_ms: u64) -> f32 {
    if held_ms < 350 {
        return 0.0;
    }
    let ramp = ((held_ms - 350) as f32 / 1_500.0).min(1.0);
    0.08 + 0.32 * ramp
}

/// How long the cursor stays where a finger left it before the live price comes back.
pub const SCRUB_HOLD_MS: u64 = 15_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tick {
    pub up: bool,
    pub at_ms: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub entry: Entry,
    pub quote: Option<Quote>,
    /// The service answered, and has no quote for this symbol.
    pub unknown: bool,
    pub tick: Option<Tick>,
    /// The source the operator chose for the list.
    pub prefer: Source,
    /// Where this symbol's prices come from: that choice, unless the service has no market for it.
    pub source: Source,
    /// The Hyperliquid coin whose prices it shows.
    pub coin: Option<String>,
    /// The coins of `entry.hl` that Hyperliquid has no market for.
    pub refused: Vec<String>,
}

impl Row {
    #[must_use]
    pub fn new(entry: Entry, prefer: Source) -> Self {
        let source = if prefer == Source::Hyperliquid && !entry.hl.is_empty() { Source::Hyperliquid } else { Source::Cnbc };
        Self { entry, quote: None, unknown: false, tick: None, prefer, source, coin: None, refused: Vec::new() }
    }

    /// What the symbol is on screen: a perpetual when Hyperliquid prices it, what CNBC calls it otherwise.
    #[must_use]
    pub fn kind(&self) -> Kind {
        let perp = match &self.quote {
            Some(q) => q.perp,
            None => self.source == Source::Hyperliquid,
        };
        if perp { Kind::Perp } else { self.entry.kind }
    }

    /// Whether CNBC is asked for this symbol: it is the symbol's source, or Hyperliquid has gone quiet.
    fn asks_cnbc(&self, hl_down: bool) -> bool {
        self.source == Source::Cnbc || hl_down
    }
}

/// How fresh the data on screen is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    /// Nothing has arrived yet.
    Loading,
    Live,
    /// The last reply is older than a poll or two.
    Late,
    /// No reply for a minute.
    Offline,
}

/// Where a finger held on the chart is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Scrub {
    /// This many pixels from the chart's left edge.
    Px(f32),
    /// This fraction of the plotting width: for the sample screens.
    #[cfg_attr(not(feature = "preview"), allow(dead_code))]
    Fraction(f32),
}

/// One symbol's chart.
#[derive(Clone, Debug, PartialEq)]
pub struct Detail {
    /// Its place in the watchlist.
    pub row: usize,
    pub frame: Frame,
    /// The bars of the frame; `None` while they load.
    pub series: Option<Series>,
    /// The last attempt to load them failed.
    pub failed: bool,
    /// The cursor: where a finger last touched the chart, and the price read there.
    pub scrub: Option<Scrub>,
    /// `uptime_ms` of the last touch of the chart.
    pub scrub_ms: u64,
    /// How many times loading the bars has failed: a symbol that Hyperliquid has not named yet is
    /// tried under each of its names in turn.
    pub attempt: usize,
}

impl Detail {
    #[must_use]
    pub fn new(row: usize, frame: Frame) -> Self {
        Self { row, frame, series: None, failed: false, scrub: None, scrub_ms: 0, attempt: 0 }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Screen {
    #[default]
    List,
    Detail(Detail),
}

#[derive(Debug, Default)]
pub struct State {
    pub rows: Vec<Row>,
    pub screen: Screen,
    /// Where the buttons of the screen last drawn are, for the next tap.
    pub hits: Option<Hits>,
    /// `uptime_ms` of the last tap.
    pub touched_ms: u64,
    /// The chart asked for in the request now in flight: the symbol's query, what it was taken for, the frame.
    pub requested: Option<(String, Kind, Frame)>,
    /// The arrow held down (-1 left, 1 right), and for how long.
    pub jog: Option<i8>,
    pub jog_ms: u64,
    /// Milliseconds the widget has been drawn for: the clock of flashes and page turns.
    pub uptime_ms: u64,
    /// Unix time of the last good reply; 0 before the first.
    pub last_ok: i64,
    /// The poll cadence now, in milliseconds.
    pub interval_ms: u32,
    /// `uptime_ms` when Hyperliquid last said anything, or when it was last asked to start.
    pub hl_heard_ms: u64,
    /// `uptime_ms` when the markets were last subscribed to.
    pub hl_since_ms: u64,
    /// Hyperliquid has said nothing for too long: CNBC prices its symbols meanwhile.
    pub hl_down: bool,
}

impl State {
    /// Takes a new watchlist, keeping what is already known about symbols that stay.
    /// A chart open on a symbol that is gone or has moved is closed. `prefer` is the source the operator chose.
    pub fn set_entries(&mut self, entries: Vec<Entry>, prefer: Source) {
        let mut old = std::mem::take(&mut self.rows);
        // A chart stays open when its symbol is still where it was, such as when only the cadence changed.
        let open = match &self.screen {
            Screen::Detail(d) => old.get(d.row).map(|r| (d.row, r.entry.clone(), r.prefer)),
            Screen::List => None,
        };
        self.rows = entries
            .into_iter()
            .map(|entry| {
                let fresh = Row::new(entry, prefer);
                match old.iter().position(|r| r.entry == fresh.entry && r.prefer == fresh.prefer) {
                    Some(i) => old.swap_remove(i),
                    None => fresh,
                }
            })
            .collect();
        let stays = open.is_some_and(|(row, entry, prefer)| self.rows.get(row).is_some_and(|r| r.entry == entry && r.prefer == prefer));
        if !stays {
            self.screen = Screen::List;
        }
        self.hl_down = false;
    }

    /// The `symbols` part of the next CNBC request: what it prices, which is everything while Hyperliquid is down.
    #[must_use]
    pub fn request_list(&self) -> String {
        let entries: Vec<Entry> = self.rows.iter().filter(|r| r.asks_cnbc(self.hl_down)).map(|r| r.entry.clone()).collect();
        request_list(&entries)
    }

    /// Whether CNBC has anything to price.
    #[must_use]
    pub fn needs_cnbc(&self) -> bool {
        self.rows.iter().any(|r| r.asks_cnbc(self.hl_down))
    }

    /// Whether Hyperliquid has anything to price.
    #[must_use]
    pub fn wants_hl(&self) -> bool {
        self.rows.iter().any(|r| r.source == Source::Hyperliquid)
    }

    /// The Hyperliquid coins to subscribe to: every name that its symbols may have there, once each.
    #[must_use]
    pub fn hl_coins(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for row in self.rows.iter().filter(|r| r.source == Source::Hyperliquid) {
            for coin in row.entry.hl.iter().filter(|c| !row.refused.contains(c)) {
                if !out.contains(coin) {
                    out.push(coin.clone());
                }
            }
        }
        out
    }

    /// The connection to Hyperliquid has just opened, and its markets are being subscribed to: it
    /// has until `hl::SILENT_MS` to say something, and `hl::PATIENCE_MS` to price each symbol.
    pub fn hl_expect(&mut self) {
        self.hl_heard_ms = self.uptime_ms;
        self.hl_since_ms = self.uptime_ms;
        self.hl_down = false;
    }

    /// The connection to Hyperliquid could not be made. With none of its prices on screen to keep, CNBC
    /// prices its symbols until it opens. Whether that changed who is asked.
    pub fn hl_failed(&mut self) -> bool {
        let down = self.hl_down || (self.wants_hl() && !self.on_hl());
        std::mem::replace(&mut self.hl_down, down) != down
    }

    /// Hyperliquid said something, whatever it was.
    pub fn hl_heard(&mut self) {
        self.hl_heard_ms = self.uptime_ms;
    }

    /// A market's prices came in. Each symbol that may be this coin takes them, unless a coin that
    /// ranks before it has answered. Whether any did.
    pub fn apply_hl(&mut self, coin: &str, ctx: &Ctx, now: i64) -> bool {
        let Some(quote) = Quote::from_hl(coin, ctx, now) else { return false };
        let shown = quote.view().price;
        let mut taken = false;
        for row in self.rows.iter_mut().filter(|r| r.source == Source::Hyperliquid) {
            let Some(rank) = row.entry.hl.iter().position(|c| c == coin) else { continue };
            let held = row.coin.as_deref().and_then(|c| row.entry.hl.iter().position(|h| h == c));
            if held.is_some_and(|have| have < rank) {
                continue;
            }
            if let Some(before) = row.quote.as_ref().map(|q| q.view().price)
                && (shown - before).abs() > f64::EPSILON {
                    row.tick = Some(Tick { up: shown > before, at_ms: self.uptime_ms });
                }
            row.coin = Some(coin.to_string());
            row.quote = Some(quote.clone());
            row.unknown = false;
            taken = true;
        }
        if taken {
            self.last_ok = now;
            self.hl_down = false;
        }
        taken
    }

    /// Hyperliquid has no market of this name. A symbol with no other name to try is asked of CNBC.
    /// Whether one was.
    pub fn refuse_hl(&mut self, coin: &str) -> bool {
        let mut moved = false;
        for row in self.rows.iter_mut().filter(|r| r.source == Source::Hyperliquid) {
            if !row.entry.hl.iter().any(|c| c == coin) {
                continue;
            }
            if !row.refused.iter().any(|c| c == coin) {
                row.refused.push(coin.to_string());
            }
            if row.coin.is_none() && row.entry.hl.iter().all(|c| row.refused.contains(c)) {
                row.source = Source::Cnbc;
                moved = true;
            }
        }
        moved
    }

    /// Looks at how Hyperliquid is doing, about once a second. Silence for too long puts its symbols
    /// on CNBC until it speaks again; a market it took in but never priced goes to CNBC for good.
    /// Whether anything changed that the sources must be told of.
    pub fn hl_watch(&mut self) -> bool {
        let silent = self.uptime_ms.saturating_sub(self.hl_heard_ms) > hl::SILENT_MS;
        let down = silent && self.wants_hl();
        let mut changed = std::mem::replace(&mut self.hl_down, down) != down;
        if !silent && self.uptime_ms.saturating_sub(self.hl_since_ms) > hl::PATIENCE_MS {
            for row in self.rows.iter_mut().filter(|r| r.source == Source::Hyperliquid && r.quote.is_none()) {
                row.source = Source::Cnbc;
                changed = true;
            }
        }
        changed
    }

    /// Takes a reply: each symbol gets its quote, or is marked unknown. A price that changed
    /// leaves a tick behind for the screen to flash.
    pub fn apply(&mut self, replies: &[Raw], now: i64) {
        let (hl_down, uptime) = (self.hl_down, self.uptime_ms);
        for row in self.rows.iter_mut().filter(|r| r.asks_cnbc(hl_down)) {
            let raw = replies.iter().find(|r| r.symbol.eq_ignore_ascii_case(&row.entry.query));
            if let Some(quote) = raw.and_then(Quote::from_raw) {
                let shown = quote.view().price;
                if let Some(before) = row.quote.as_ref().map(|q| q.view().price)
                    && (shown - before).abs() > f64::EPSILON {
                        row.tick = Some(Tick { up: shown > before, at_ms: uptime });
                    }
                row.quote = Some(quote);
                row.unknown = false;
            } else {
                // Keep the old quote when the symbol was simply left out of one reply.
                row.unknown = raw.is_some() || row.quote.is_none() && !replies.is_empty();
                if row.unknown {
                    row.quote = None;
                }
            }
        }
        self.last_ok = now;
    }

    /// A tap at `x`, `y` of the screen last drawn. Whether it changed the screen.
    pub fn on_tap(&mut self, x: f32, y: f32) -> bool {
        self.touched_ms = self.uptime_ms;
        let Some(hits) = &self.hits else { return false };
        match resolve(hits, x, y) {
            Tap::Open(i) if i < self.rows.len() => {
                self.screen = Screen::Detail(Detail::new(i, Frame::Day));
                true
            }
            Tap::Nothing | Tap::Open(_) => false,
            Tap::Back => std::mem::take(&mut self.screen) != Screen::List,
            Tap::Frame(frame) => match &mut self.screen {
                Screen::Detail(d) if d.frame != frame => {
                    *d = Detail::new(d.row, frame);
                    true
                }
                _ => false,
            },
            Tap::Live => match &mut self.screen {
                Screen::Detail(d) => d.scrub.take().is_some(),
                Screen::List => false,
            },
            Tap::Step(delta) => match &mut self.screen {
                Screen::Detail(d) if !self.rows.is_empty() => {
                    let n = self.rows.len() as isize;
                    let next = (d.row as isize + delta).rem_euclid(n) as usize;
                    *d = Detail::new(next, d.frame);
                    true
                }
                _ => false,
            },
        }
    }

    /// A finger is on the chart at `x` pixels from its left edge (`None`: there is none). The cursor goes
    /// there, and stays where the finger leaves it until `expire_scrub`, a tap on Live, or a change of
    /// frame or symbol: the Deck turns any slide of more than a few pixels into a swipe of the scene, so
    /// the chart is read by touching it, not by sliding along it. Whether the cursor moved.
    pub fn set_scrub(&mut self, x: Option<f32>) -> bool {
        let Screen::Detail(d) = &mut self.screen else { return false };
        let Some(x) = x else { return false };
        self.touched_ms = self.uptime_ms;
        d.scrub_ms = self.uptime_ms;
        let now = Some(Scrub::Px(x));
        std::mem::replace(&mut d.scrub, now) != now
    }

    /// An arrow of the chart is held (`held` is -1 or 1, or `None` when no arrow is), for `dt_ms` since
    /// the last frame, over a plotting box `pw` wide. The cursor moves along the chart: a notch as the
    /// finger comes down, then, past a short wait, faster the longer it is held. With no cursor yet it
    /// starts from the right end, which is now. This is how the chart is swept without a slide, which
    /// the Deck would take for a swipe of the scene. Whether anything changed on screen.
    pub fn jog(&mut self, held: Option<i8>, dt_ms: u64, pw: f32) -> bool {
        let Screen::Detail(d) = &mut self.screen else {
            return std::mem::take(&mut self.jog).is_some();
        };
        let Some(dir) = held else {
            self.jog_ms = 0;
            return std::mem::take(&mut self.jog).is_some();
        };
        let first = self.jog != Some(dir);
        if first {
            self.jog = Some(dir);
            self.jog_ms = 0;
        } else {
            self.jog_ms += dt_ms;
        }
        let from = match d.scrub {
            Some(Scrub::Px(x)) => x,
            Some(Scrub::Fraction(f)) => f * pw,
            None => pw,
        };
        let step = if first { pw / 240.0 } else { jog_speed(self.jog_ms) * pw * dt_ms as f32 / 1_000.0 };
        self.touched_ms = self.uptime_ms;
        d.scrub_ms = self.uptime_ms;
        d.scrub = Some(Scrub::Px((from + f32::from(dir) * step).clamp(0.0, pw)));
        true
    }

    /// Lets the cursor go when nobody has touched the chart for a while. Whether it did.
    pub fn expire_scrub(&mut self) -> bool {
        let Screen::Detail(d) = &mut self.screen else { return false };
        let held = matches!(d.scrub, Some(Scrub::Px(_))) && self.uptime_ms.saturating_sub(d.scrub_ms) >= SCRUB_HOLD_MS;
        if held {
            d.scrub = None;
        }
        held
    }

    /// The chart to fetch: the symbol's query and kind, and the time frame; `None` on the list.
    #[must_use]
    pub fn chart_target(&self) -> Option<(String, Kind, Frame)> {
        let Screen::Detail(d) = &self.screen else { return None };
        let row = self.rows.get(d.row)?;
        Some((row.entry.query.clone(), row.kind(), d.frame))
    }

    /// The Hyperliquid coin to take the open chart's candles from: the one that prices the symbol, or
    /// until one does, each name it may have there in turn.
    #[must_use]
    pub fn chart_coin(&self) -> Option<String> {
        let Screen::Detail(d) = &self.screen else { return None };
        let row = self.rows.get(d.row)?;
        let names: Vec<&String> = row.entry.hl.iter().filter(|c| !row.refused.contains(c)).collect();
        row.coin.clone().or_else(|| names.get(d.attempt % names.len().max(1)).map(|name| (*name).clone()))
    }

    /// Takes the bars that came back, if they are still the ones on show.
    pub fn accept_chart(&mut self, query: &str, series: Series) -> bool {
        match &mut self.screen {
            Screen::Detail(d) if d.frame == series.frame && self.rows.get(d.row).is_some_and(|r| r.entry.query == query) => {
                d.series = Some(series);
                d.failed = false;
                true
            }
            _ => false,
        }
    }

    /// The bars for this symbol and frame could not be read.
    pub fn fail_chart(&mut self, query: &str, frame: Frame) {
        if let Screen::Detail(d) = &mut self.screen
            && d.frame == frame && self.rows.get(d.row).is_some_and(|r| r.entry.query == query) {
                d.attempt += 1;
                d.failed |= d.series.is_none();
            }
    }

    /// Back to the list when nobody has touched a chart for a while. Whether it did.
    pub fn close_if_idle(&mut self) -> bool {
        let idle = self.uptime_ms.saturating_sub(self.touched_ms) >= DETAIL_IDLE_MS;
        idle && std::mem::take(&mut self.screen) != Screen::List
    }

    /// Whether any symbol of CNBC's is trading now (pre-market, regular or after-hours): how often to ask it.
    #[must_use]
    pub fn any_trading(&self) -> bool {
        self.rows.iter().filter_map(|r| r.quote.as_ref()).any(|q| !q.perp && q.session != Session::Closed)
    }

    /// The session that leads the header: that of the US stock market by the New York clock at `now`
    /// (Unix seconds), when CNBC prices any stock or index. CNBC's own status field is no guide, and a
    /// perpetual trades round the clock and has none.
    #[must_use]
    pub fn market_session(&self, now: i64) -> Option<Session> {
        let priced = |r: &&Row| matches!(r.entry.kind, Kind::Stock | Kind::Index) && r.quote.as_ref().is_some_and(|q| !q.perp);
        self.rows.iter().find(priced).map(|_| clock::us_session(now))
    }

    /// Whether any price on screen comes from Hyperliquid.
    #[must_use]
    pub fn on_hl(&self) -> bool {
        self.rows.iter().any(|r| r.quote.as_ref().is_some_and(|q| q.perp))
    }

    #[must_use]
    pub fn health(&self, now: i64) -> Health {
        if self.last_ok == 0 {
            return Health::Loading;
        }
        let age = now - self.last_ok;
        let late_after = i64::from(self.interval_ms) / 1000 * 2 + 8;
        if age <= late_after {
            Health::Live
        } else if age <= 60 + i64::from(self.interval_ms) / 1000 {
            Health::Late
        } else {
            Health::Offline
        }
    }

    /// Whether any price is still flashing.
    #[must_use]
    pub fn flashing(&self) -> bool {
        self.rows
            .iter()
            .filter_map(|r| r.tick)
            .any(|t| self.uptime_ms.saturating_sub(t.at_ms) < FLASH_MS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbols::resolve_all;

    fn raw(symbol: &str, last: &str) -> Raw {
        Raw {
            symbol: symbol.into(),
            last: last.into(),
            prev_close: "100".into(),
            change_pct: "+1.00%".into(),
            status: "REG_MKT".into(),
            real_time: "true".into(),
            ..Raw::default()
        }
    }

    fn state(symbols: &[&str]) -> State {
        let mut s = State { interval_ms: 2000, ..State::default() };
        s.set_entries(resolve_all(symbols), Source::Cnbc);
        s
    }

    #[test]
    fn a_reply_fills_the_quotes_and_marks_unknown_symbols() {
        let mut s = state(&["NVDA", "APPL"]);
        let mut unknown = raw("APPL", "");
        unknown.code = 1;
        s.apply(&[raw("NVDA", "167.29"), unknown], 1000);
        assert!(s.rows[0].quote.is_some());
        assert!(!s.rows[0].unknown);
        assert!(s.rows[1].quote.is_none());
        assert!(s.rows[1].unknown);
        assert_eq!(s.last_ok, 1000);
    }

    #[test]
    fn a_price_that_moves_leaves_a_tick_and_one_that_stays_does_not() {
        let mut s = state(&["NVDA"]);
        s.apply(&[raw("NVDA", "167.29")], 1000);
        assert_eq!(s.rows[0].tick, None);
        s.uptime_ms = 5000;
        s.apply(&[raw("NVDA", "167.30")], 1002);
        assert_eq!(s.rows[0].tick, Some(Tick { up: true, at_ms: 5000 }));
        assert!(s.flashing());
        s.uptime_ms = 6000;
        assert!(!s.flashing());
        s.apply(&[raw("NVDA", "167.30")], 1004);
        assert_eq!(s.rows[0].tick, Some(Tick { up: true, at_ms: 5000 }));
        s.apply(&[raw("NVDA", "167.10")], 1006);
        assert_eq!(s.rows[0].tick, Some(Tick { up: false, at_ms: 6000 }));
    }

    #[test]
    fn a_symbol_left_out_of_one_reply_keeps_its_quote() {
        let mut s = state(&["NVDA", "TSLA"]);
        s.apply(&[raw("NVDA", "167.29"), raw("TSLA", "389.63")], 1000);
        s.apply(&[raw("NVDA", "167.40")], 1002);
        assert!(s.rows[1].quote.is_some());
        assert!(!s.rows[1].unknown);
    }

    #[test]
    fn a_new_list_keeps_what_the_staying_symbols_knew() {
        let mut s = state(&["NVDA", "TSLA"]);
        s.apply(&[raw("NVDA", "167.29"), raw("TSLA", "389.63")], 1000);
        s.set_entries(resolve_all(&["TSLA", "META"]), Source::Cnbc);
        assert_eq!(s.rows.len(), 2);
        assert!(s.rows[0].quote.is_some());
        assert!(s.rows[1].quote.is_none());
    }

    #[test]
    fn health_follows_the_age_of_the_last_reply() {
        let mut s = state(&["NVDA"]);
        assert_eq!(s.health(1000), Health::Loading);
        s.apply(&[raw("NVDA", "167.29")], 1000);
        assert_eq!(s.health(1005), Health::Live);
        assert_eq!(s.health(1030), Health::Late);
        assert_eq!(s.health(1200), Health::Offline);
    }

    #[test]
    fn the_request_lists_every_symbol_encoded() {
        assert_eq!(state(&["NVDA", "BTC-USD"]).request_list(), "NVDA%7CBTC.CM%3D");
    }

    use crate::chart::{Point, Phase};
    use crate::view::Rect;

    fn with_cells(s: &mut State, n: usize) {
        // Cells of 100 by 50, one above the other.
        s.hits = Some(Hits::List { cells: (0..n).map(|k| Rect::new(0.0, 60.0 * k as f32, 100.0, 50.0)).collect(), first: 0 });
    }

    fn series(frame: Frame) -> Series {
        let p = |i: i64| Point { ms: i * 60_000, local: i * 60, close: 10.0 + i as f64, phase: Phase::Regular };
        Series { frame, kind: Kind::Stock, points: vec![p(0), p(1), p(2)] }
    }

    #[test]
    fn a_tap_on_a_cell_opens_its_chart_on_the_day() {
        let mut s = state(&["NVDA", "TSLA"]);
        with_cells(&mut s, 2);
        assert!(s.on_tap(50.0, 85.0));
        assert_eq!(s.screen, Screen::Detail(Detail::new(1, Frame::Day)));
        assert_eq!(s.chart_target(), Some(("TSLA".to_string(), Kind::Stock, Frame::Day)));
    }

    #[test]
    fn a_tap_beside_the_cells_changes_nothing_but_counts_as_a_touch() {
        let mut s = state(&["NVDA"]);
        with_cells(&mut s, 1);
        s.uptime_ms = 5_000;
        assert!(!s.on_tap(500.0, 500.0));
        assert_eq!(s.screen, Screen::List);
        assert_eq!(s.touched_ms, 5_000);
    }

    #[test]
    fn the_frame_buttons_change_the_frame_and_drop_the_old_bars() {
        let mut s = state(&["NVDA", "TSLA"]);
        s.screen = Screen::Detail(Detail { series: Some(series(Frame::Day)), ..Detail::new(0, Frame::Day) });
        let layout = crate::view::detail_layout(1280.0, 480.0);
        let r = layout.frames[2];
        s.hits = Some(Hits::Detail(layout));
        assert!(s.on_tap(r.x + 5.0, r.y + 5.0));
        let Screen::Detail(d) = &s.screen else { panic!("BUG: still on a chart") };
        assert_eq!((d.row, d.frame, d.series.is_none()), (0, Frame::Month, true));
        // The frame that is already on show is not loaded again.
        assert!(!s.on_tap(r.x + 5.0, r.y + 5.0));
    }

    #[test]
    fn the_arrows_step_through_the_list_and_wrap_round() {
        let mut s = state(&["NVDA", "TSLA", "META"]);
        s.screen = Screen::Detail(Detail::new(2, Frame::Month));
        let layout = crate::view::detail_layout(1280.0, 480.0);
        let (prev, next) = (layout.prev.expect("BUG: arrows"), layout.next.expect("BUG: arrows"));
        s.hits = Some(Hits::Detail(layout));
        assert!(s.on_tap(next.x + 5.0, next.y + 5.0));
        assert_eq!(s.screen, Screen::Detail(Detail::new(0, Frame::Month)));
        assert!(s.on_tap(prev.x + 5.0, prev.y + 5.0));
        assert_eq!(s.screen, Screen::Detail(Detail::new(2, Frame::Month)));
    }

    #[test]
    fn back_returns_to_the_list() {
        let mut s = state(&["NVDA"]);
        s.screen = Screen::Detail(Detail::new(0, Frame::Day));
        let layout = crate::view::detail_layout(1280.0, 480.0);
        let back = layout.back;
        s.hits = Some(Hits::Detail(layout));
        assert!(s.on_tap(back.x + 5.0, back.y + 5.0));
        assert_eq!(s.screen, Screen::List);
        assert_eq!(s.chart_target(), None);
    }

    #[test]
    fn bars_are_taken_only_for_the_chart_on_show() {
        let mut s = state(&["NVDA", "TSLA"]);
        s.screen = Screen::Detail(Detail::new(0, Frame::Week));
        assert!(!s.accept_chart("TSLA", series(Frame::Week)), "another symbol");
        assert!(!s.accept_chart("NVDA", series(Frame::Day)), "another frame");
        assert!(s.accept_chart("NVDA", series(Frame::Week)));
        let Screen::Detail(d) = &s.screen else { panic!("BUG: still on a chart") };
        assert!(d.series.is_some() && !d.failed);
        s.screen = Screen::List;
        assert!(!s.accept_chart("NVDA", series(Frame::Week)));
    }

    #[test]
    fn a_failed_load_is_marked_unless_there_are_bars_already() {
        let mut s = state(&["NVDA"]);
        s.screen = Screen::Detail(Detail::new(0, Frame::Day));
        s.fail_chart("NVDA", Frame::Day);
        assert!(matches!(&s.screen, Screen::Detail(d) if d.failed));
        s.screen = Screen::Detail(Detail { series: Some(series(Frame::Day)), ..Detail::new(0, Frame::Day) });
        s.fail_chart("NVDA", Frame::Day);
        assert!(matches!(&s.screen, Screen::Detail(d) if !d.failed), "the bars on show stay");
    }

    #[test]
    fn a_chart_gives_way_to_the_list_after_a_while_without_taps() {
        let mut s = state(&["NVDA"]);
        s.screen = Screen::Detail(Detail::new(0, Frame::Day));
        s.touched_ms = 1_000;
        s.uptime_ms = 1_000 + DETAIL_IDLE_MS - 1;
        assert!(!s.close_if_idle());
        s.uptime_ms += 1;
        assert!(s.close_if_idle());
        assert_eq!(s.screen, Screen::List);
        assert!(!s.close_if_idle());
    }

    #[test]
    fn a_new_watchlist_closes_the_chart() {
        let mut s = state(&["NVDA", "TSLA"]);
        s.screen = Screen::Detail(Detail::new(1, Frame::Day));
        s.set_entries(resolve_all(&["NVDA"]), Source::Cnbc);
        assert_eq!(s.screen, Screen::List);
    }

    #[test]
    fn the_cursor_goes_where_a_finger_touches_and_stays_when_it_leaves() {
        let mut s = state(&["NVDA"]);
        assert!(!s.set_scrub(Some(10.0)), "the list has no chart to read");
        s.screen = Screen::Detail(Detail::new(0, Frame::Day));
        s.uptime_ms = 7_000;
        assert!(s.set_scrub(Some(10.0)));
        assert_eq!(s.touched_ms, 7_000, "a touch of the chart keeps it open");
        assert!(!s.set_scrub(Some(10.0)), "the same place changes nothing");
        assert!(s.set_scrub(Some(12.0)));
        assert!(!s.set_scrub(None), "a finger leaving is not a change");
        assert!(matches!(&s.screen, Screen::Detail(d) if d.scrub == Some(Scrub::Px(12.0))));
    }

    #[test]
    fn the_cursor_lets_go_after_a_while_but_not_a_sample_one() {
        let mut s = state(&["NVDA"]);
        s.screen = Screen::Detail(Detail::new(0, Frame::Day));
        s.uptime_ms = 1_000;
        s.set_scrub(Some(40.0));
        s.uptime_ms = 1_000 + SCRUB_HOLD_MS - 1;
        assert!(!s.expire_scrub());
        s.uptime_ms += 1;
        assert!(s.expire_scrub());
        assert!(matches!(&s.screen, Screen::Detail(d) if d.scrub.is_none()));
        assert!(!s.expire_scrub());
        if let Screen::Detail(d) = &mut s.screen {
            d.scrub = Some(Scrub::Fraction(0.5));
        }
        s.uptime_ms += 10 * SCRUB_HOLD_MS;
        assert!(!s.expire_scrub(), "a cursor of a sample screen stays");
    }

    #[test]
    fn the_live_button_lets_the_cursor_go() {
        let mut s = state(&["NVDA"]);
        s.screen = Screen::Detail(Detail { scrub: Some(Scrub::Px(30.0)), ..Detail::new(0, Frame::Day) });
        let mut layout = crate::view::detail_layout(1280.0, 480.0);
        layout.live = Some(Rect::new(1000.0, 400.0, 150.0, 48.0));
        s.hits = Some(Hits::Detail(layout));
        assert!(s.on_tap(1050.0, 420.0));
        assert!(matches!(&s.screen, Screen::Detail(d) if d.scrub.is_none()));
        assert!(!s.on_tap(1050.0, 420.0), "nothing to let go of any more");
    }

    #[test]
    fn changing_the_frame_lets_go_of_the_cursor() {
        let mut s = state(&["NVDA"]);
        s.screen = Screen::Detail(Detail { scrub: Some(Scrub::Px(30.0)), ..Detail::new(0, Frame::Day) });
        let layout = crate::view::detail_layout(1280.0, 480.0);
        let r = layout.frames[1];
        s.hits = Some(Hits::Detail(layout));
        assert!(s.on_tap(r.x + 5.0, r.y + 5.0));
        assert!(matches!(&s.screen, Screen::Detail(d) if d.scrub.is_none()));
    }

    #[test]
    fn an_arrow_moves_the_cursor_a_notch_then_sweeps_faster_the_longer_it_is_held() {
        let mut s = state(&["NVDA"]);
        s.screen = Screen::Detail(Detail::new(0, Frame::Day));
        s.uptime_ms = 1_000;
        let x = |s: &State| match &s.screen {
            Screen::Detail(d) => match d.scrub {
                Some(Scrub::Px(x)) => x,
                _ => f32::NAN,
            },
            Screen::List => f32::NAN,
        };
        // No cursor yet: it starts at the right end, and a notch left of it is one point's width.
        assert!(s.jog(Some(-1), 16, 1200.0));
        assert!((x(&s) - (1200.0 - 5.0)).abs() < 0.01, "{}", x(&s));
        // Held, but not for long: still.
        assert!(s.jog(Some(-1), 100, 1200.0));
        assert!((x(&s) - 1195.0).abs() < 0.01);
        // Held on: it sweeps, and faster later.
        for _ in 0..30 {
            s.jog(Some(-1), 16, 1200.0);
        }
        let early = x(&s);
        assert!(early < 1195.0 - 20.0, "it moved: {early}");
        let before_fast = x(&s);
        for _ in 0..60 {
            s.jog(Some(-1), 16, 1200.0);
        }
        let slow_step = (1195.0 - early) / 30.0;
        let fast_step = (before_fast - x(&s)) / 60.0;
        assert!(fast_step > slow_step, "{fast_step} vs {slow_step}");
        // Letting go ends the hold; the cursor stays.
        let at = x(&s);
        assert!(s.jog(None, 16, 1200.0), "the arrow's highlight goes");
        assert!(!s.jog(None, 16, 1200.0));
        assert_eq!(x(&s), at);
        // The other way, from where it is.
        assert!(s.jog(Some(1), 16, 1200.0));
        assert!(x(&s) > at);
    }

    #[test]
    fn the_cursor_stops_at_the_ends_and_an_arrow_does_nothing_on_the_list() {
        let mut s = state(&["NVDA"]);
        assert!(!s.jog(Some(1), 16, 1200.0), "the list has no cursor");
        s.screen = Screen::Detail(Detail { scrub: Some(Scrub::Px(2.0)), ..Detail::new(0, Frame::Day) });
        for _ in 0..200 {
            s.jog(Some(-1), 50, 1200.0);
        }
        assert!(matches!(&s.screen, Screen::Detail(d) if d.scrub == Some(Scrub::Px(0.0))));
        for _ in 0..400 {
            s.jog(Some(1), 50, 1200.0);
        }
        assert!(matches!(&s.screen, Screen::Detail(d) if d.scrub == Some(Scrub::Px(1200.0))));
    }

    #[test]
    fn an_arrow_keeps_the_chart_and_its_cursor_open() {
        let mut s = state(&["NVDA"]);
        s.screen = Screen::Detail(Detail::new(0, Frame::Day));
        s.uptime_ms = 40_000;
        s.jog(Some(-1), 16, 1200.0);
        assert_eq!(s.touched_ms, 40_000);
        assert!(matches!(&s.screen, Screen::Detail(d) if d.scrub_ms == 40_000));
    }

    // ── Hyperliquid ─────────────────────────────────────────────────

    fn hl_state(symbols: &[&str]) -> State {
        let mut s = State { interval_ms: 2000, ..State::default() };
        s.set_entries(resolve_all(symbols), Source::Hyperliquid);
        s
    }

    fn ctx(mark: &str) -> Ctx {
        Ctx { mark: mark.into(), mid: String::new(), prev_day: "100".into(), volume: "1000000".into() }
    }

    #[test]
    fn symbols_with_a_market_there_come_from_hyperliquid_and_the_rest_from_cnbc() {
        let s = hl_state(&["NVDA", "BRK.B", "^DJI", "BTC-USD"]);
        let sources: Vec<Source> = s.rows.iter().map(|r| r.source).collect();
        assert_eq!(sources, [Source::Hyperliquid, Source::Cnbc, Source::Cnbc, Source::Hyperliquid]);
        assert_eq!(s.request_list(), "BRK.B%7C.DJI");
        assert_eq!(s.hl_coins(), ["NVDA", "xyz:NVDA", "BTC"]);
        assert!(s.needs_cnbc() && s.wants_hl());
    }

    #[test]
    fn the_cnbc_choice_leaves_nothing_to_hyperliquid() {
        let mut s = State::default();
        s.set_entries(resolve_all(&["NVDA", "BTC-USD"]), Source::Cnbc);
        assert!(!s.wants_hl() && s.hl_coins().is_empty());
        assert_eq!(s.request_list(), "NVDA%7CBTC.CM%3D");
    }

    #[test]
    fn a_market_that_answers_gives_its_symbol_a_perpetual_quote_and_a_tick() {
        let mut s = hl_state(&["NVDA"]);
        s.uptime_ms = 4_000;
        assert!(s.apply_hl("xyz:NVDA", &ctx("163.43"), 1_000));
        assert_eq!(s.rows[0].coin.as_deref(), Some("xyz:NVDA"));
        assert_eq!(s.rows[0].kind(), Kind::Perp);
        assert_eq!((s.last_ok, s.rows[0].tick), (1_000, None));
        s.uptime_ms = 5_000;
        s.apply_hl("xyz:NVDA", &ctx("163.50"), 1_001);
        assert_eq!(s.rows[0].tick, Some(Tick { up: true, at_ms: 5_000 }));
    }

    #[test]
    fn a_name_that_ranks_first_wins_once_it_has_answered() {
        let mut s = hl_state(&["NVDA"]);
        s.apply_hl("xyz:NVDA", &ctx("163.0"), 1);
        assert_eq!(s.rows[0].coin.as_deref(), Some("xyz:NVDA"));
        s.apply_hl("NVDA", &ctx("1.0"), 2);
        assert_eq!(s.rows[0].coin.as_deref(), Some("NVDA"), "the main market's name ranks first");
        s.apply_hl("xyz:NVDA", &ctx("163.1"), 3);
        assert_eq!(s.rows[0].coin.as_deref(), Some("NVDA"), "and keeps the symbol");
    }

    #[test]
    fn a_coin_nobody_listed_or_without_a_price_is_ignored() {
        let mut s = hl_state(&["NVDA"]);
        assert!(!s.apply_hl("xyz:TSLA", &ctx("1.0"), 1));
        assert!(!s.apply_hl("xyz:NVDA", &Ctx::default(), 1));
        assert!(s.rows[0].quote.is_none());
        assert_eq!(s.last_ok, 0);
    }

    #[test]
    fn a_symbol_whose_names_are_all_refused_falls_back_to_cnbc() {
        let mut s = hl_state(&["JPM", "NVDA"]);
        assert!(!s.refuse_hl("JPM"), "one name is not all of them");
        assert_eq!(s.rows[0].source, Source::Hyperliquid);
        assert!(s.refuse_hl("xyz:JPM"));
        assert_eq!(s.rows[0].source, Source::Cnbc);
        assert_eq!(s.request_list(), "JPM");
        assert_eq!(s.hl_coins(), ["NVDA", "xyz:NVDA"]);
        // NVDA is refused on the main market only, and answers on the builder's.
        assert!(!s.refuse_hl("NVDA"));
        s.apply_hl("xyz:NVDA", &ctx("163.0"), 1);
        assert_eq!(s.rows[1].source, Source::Hyperliquid);
    }

    #[test]
    fn a_refusal_does_not_take_a_symbol_that_has_answered() {
        let mut s = hl_state(&["NVDA"]);
        s.apply_hl("xyz:NVDA", &ctx("163.0"), 1);
        assert!(!s.refuse_hl("NVDA") && !s.refuse_hl("xyz:NVDA"));
        assert_eq!(s.rows[0].source, Source::Hyperliquid);
    }

    #[test]
    fn silence_puts_cnbc_in_charge_until_hyperliquid_speaks_again() {
        let mut s = hl_state(&["NVDA", "BRK.B"]);
        s.hl_expect();
        s.apply_hl("xyz:NVDA", &ctx("163.0"), 1);
        s.uptime_ms = hl::SILENT_MS;
        assert!(!s.hl_watch(), "not yet");
        assert_eq!(s.request_list(), "BRK.B");
        s.uptime_ms += 1;
        assert!(s.hl_watch());
        assert_eq!(s.request_list(), "NVDA%7CBRK.B");
        assert!(!s.hl_watch(), "nothing new");
        s.hl_heard();
        assert!(s.hl_watch());
        assert_eq!(s.request_list(), "BRK.B");
    }

    #[test]
    fn a_market_that_never_prices_goes_to_cnbc_after_a_while() {
        let mut s = hl_state(&["NVDA", "TSLA"]);
        s.hl_expect();
        s.apply_hl("xyz:TSLA", &ctx("390.0"), 1);
        s.uptime_ms = hl::PATIENCE_MS;
        s.hl_heard();
        assert!(!s.hl_watch());
        s.uptime_ms += 1;
        s.hl_heard();
        assert!(s.hl_watch());
        assert_eq!((s.rows[0].source, s.rows[1].source), (Source::Cnbc, Source::Hyperliquid));
        assert_eq!(s.request_list(), "NVDA");
    }

    #[test]
    fn a_perpetual_leaves_the_header_and_the_cnbc_cadence_alone() {
        let mut s = hl_state(&["NVDA"]);
        s.apply_hl("xyz:NVDA", &ctx("163.0"), 1);
        assert_eq!(s.market_session(1_795_000_000), None);
        assert!(!s.any_trading());
        assert!(s.on_hl());
    }

    #[test]
    fn the_chart_of_a_perpetual_names_its_coin() {
        let mut s = hl_state(&["NVDA"]);
        s.screen = Screen::Detail(Detail::new(0, Frame::Day));
        assert_eq!(s.chart_target(), Some(("NVDA".to_string(), Kind::Perp, Frame::Day)));
        assert_eq!(s.chart_coin().as_deref(), Some("NVDA"), "the first name, until one answers");
        s.fail_chart("NVDA", Frame::Day);
        assert_eq!(s.chart_coin().as_deref(), Some("xyz:NVDA"), "and then the next, if the first had no candles");
        s.fail_chart("NVDA", Frame::Day);
        assert_eq!(s.chart_coin().as_deref(), Some("NVDA"), "round again");
        s.apply_hl("xyz:NVDA", &ctx("163.0"), 1);
        assert_eq!(s.chart_coin().as_deref(), Some("xyz:NVDA"), "a market that prices the symbol is the one");
    }

    #[test]
    fn a_new_list_keeps_what_stays_and_forgets_a_changed_source() {
        let mut s = hl_state(&["NVDA", "TSLA"]);
        s.apply_hl("xyz:NVDA", &ctx("163.0"), 1);
        s.set_entries(resolve_all(&["NVDA", "META"]), Source::Hyperliquid);
        assert!(s.rows[0].quote.is_some());
        assert!(s.rows[1].quote.is_none());
        s.set_entries(resolve_all(&["NVDA"]), Source::Cnbc);
        assert!(s.rows[0].quote.is_none(), "another source, another price");
    }

    #[test]
    fn a_connection_that_cannot_be_made_hands_the_symbols_to_cnbc_until_one_opens() {
        let mut s = hl_state(&["NVDA", "BRK.B"]);
        assert!(s.hl_failed());
        assert_eq!(s.request_list(), "NVDA%7CBRK.B");
        assert!(!s.hl_failed(), "already so");
        s.hl_expect();
        assert_eq!(s.request_list(), "BRK.B", "an open connection has its symbols back");
    }

    #[test]
    fn prices_already_on_screen_are_kept_when_a_new_connection_fails() {
        let mut s = hl_state(&["NVDA"]);
        s.apply_hl("xyz:NVDA", &ctx("163.0"), 1);
        assert!(!s.hl_failed());
        assert!(!s.needs_cnbc());
    }

    #[test]
    fn a_chart_stays_open_when_its_symbol_is_untouched_by_a_new_list() {
        let mut s = hl_state(&["NVDA", "TSLA"]);
        s.screen = Screen::Detail(Detail::new(1, Frame::Week));
        s.set_entries(resolve_all(&["NVDA", "TSLA", "META"]), Source::Hyperliquid);
        assert_eq!(s.screen, Screen::Detail(Detail::new(1, Frame::Week)), "TSLA is still second");
        s.set_entries(resolve_all(&["TSLA", "NVDA"]), Source::Hyperliquid);
        assert_eq!(s.screen, Screen::List, "it moved");
        s.screen = Screen::Detail(Detail::new(0, Frame::Day));
        s.set_entries(resolve_all(&["TSLA", "NVDA"]), Source::Cnbc);
        assert_eq!(s.screen, Screen::List, "another source");
    }

    #[test]
    fn the_header_session_follows_the_new_york_clock_for_stocks_priced_by_cnbc() {
        let mut s = state(&["NVDA", "BTC-USD"]);
        assert_eq!(s.market_session(1_795_000_000), None, "no quote yet");
        s.apply(&[raw("NVDA", "167.29")], 1000);
        for now in [1_795_000_000, 1_795_040_000, 1_795_100_000] {
            assert_eq!(s.market_session(now), Some(clock::us_session(now)));
        }
        // Coins alone have no stock market to name.
        let mut coins = state(&["BTC-USD"]);
        coins.apply(&[raw("BTC.CM=", "60000")], 1000);
        assert_eq!(coins.market_session(1_795_000_000), None);
    }
}
