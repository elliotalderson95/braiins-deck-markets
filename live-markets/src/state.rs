//! What the widget knows: the watchlist, the latest quote of each symbol, and how fresh they are.

use crate::chart::{Frame, Series};
use crate::quote::{Quote, Raw, Session};
use crate::symbols::{Entry, Kind, request_list};
use crate::view::{Hits, Tap, resolve};

/// How long a price flashes green or red after it ticks.
pub const FLASH_MS: u64 = 900;
/// How long a chart stays up after the last tap before the list comes back.
pub const DETAIL_IDLE_MS: u64 = 90_000;

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
    /// A finger is down on the chart, reading the price at that time.
    pub scrub: Option<Scrub>,
}

impl Detail {
    #[must_use]
    pub fn new(row: usize, frame: Frame) -> Self {
        Self { row, frame, series: None, failed: false, scrub: None }
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
    /// The chart asked for in the request now in flight.
    pub requested: Option<(String, Frame)>,
    /// Milliseconds the widget has been drawn for: the clock of flashes and page turns.
    pub uptime_ms: u64,
    /// Unix time of the last good reply; 0 before the first.
    pub last_ok: i64,
    /// The poll cadence now, in milliseconds.
    pub interval_ms: u32,
}

impl State {
    /// Takes a new watchlist, keeping what is already known about symbols that stay.
    /// A chart open on the old one is closed.
    pub fn set_entries(&mut self, entries: Vec<Entry>) {
        self.screen = Screen::List;
        let mut old = std::mem::take(&mut self.rows);
        self.rows = entries
            .into_iter()
            .map(|entry| match old.iter().position(|r| r.entry.query == entry.query) {
                Some(i) => Row { entry, ..old.swap_remove(i) },
                None => Row { entry, quote: None, unknown: false, tick: None },
            })
            .collect();
    }

    /// The `symbols` part of the next request.
    #[must_use]
    pub fn request_list(&self) -> String {
        let entries: Vec<Entry> = self.rows.iter().map(|r| r.entry.clone()).collect();
        request_list(&entries)
    }

    /// Takes a reply: each symbol gets its quote, or is marked unknown. A price that changed
    /// leaves a tick behind for the screen to flash.
    pub fn apply(&mut self, replies: &[Raw], now: i64) {
        for row in &mut self.rows {
            let raw = replies.iter().find(|r| r.symbol.eq_ignore_ascii_case(&row.entry.query));
            if let Some(quote) = raw.and_then(Quote::from_raw) {
                let shown = quote.view().price;
                if let Some(before) = row.quote.as_ref().map(|q| q.view().price)
                    && (shown - before).abs() > f64::EPSILON {
                        row.tick = Some(Tick { up: shown > before, at_ms: self.uptime_ms });
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

    /// A finger is on the chart at `x` pixels from its left edge, or has left it (`None`).
    /// Whether that changed what is on show. A finger on the chart counts as a touch.
    pub fn set_scrub(&mut self, x: Option<f32>) -> bool {
        let Screen::Detail(d) = &mut self.screen else { return false };
        let now = x.map(Scrub::Px);
        if now.is_none() && matches!(d.scrub, Some(Scrub::Fraction(_))) {
            // A cursor set for a sample screen stays until a finger comes.
            return false;
        }
        if now.is_some() {
            self.touched_ms = self.uptime_ms;
        }
        std::mem::replace(&mut d.scrub, now) != now
    }

    /// The chart to fetch: the symbol's query and kind, and the time frame; `None` on the list.
    #[must_use]
    pub fn chart_target(&self) -> Option<(String, Kind, Frame)> {
        let Screen::Detail(d) = &self.screen else { return None };
        let row = self.rows.get(d.row)?;
        Some((row.entry.query.clone(), row.entry.kind, d.frame))
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
            && d.frame == frame && self.rows.get(d.row).is_some_and(|r| r.entry.query == query) && d.series.is_none() {
                d.failed = true;
            }
    }

    /// Back to the list when nobody has touched a chart for a while. Whether it did.
    pub fn close_if_idle(&mut self) -> bool {
        let idle = self.uptime_ms.saturating_sub(self.touched_ms) >= DETAIL_IDLE_MS;
        idle && std::mem::take(&mut self.screen) != Screen::List
    }

    /// Whether any symbol is trading now (pre-market, regular or after-hours).
    #[must_use]
    pub fn any_trading(&self) -> bool {
        self.rows.iter().filter_map(|r| r.quote.as_ref()).any(|q| q.session != Session::Closed)
    }

    /// The session that leads the header: the first stock, index or future that has a quote.
    #[must_use]
    pub fn market_session(&self) -> Option<Session> {
        use crate::symbols::Kind;
        self.rows
            .iter()
            .filter(|r| r.entry.kind != Kind::Crypto)
            .find_map(|r| r.quote.as_ref().map(|q| q.session))
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
        s.set_entries(resolve_all(symbols));
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
        s.set_entries(resolve_all(&["TSLA", "META"]));
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
        s.set_entries(resolve_all(&["NVDA"]));
        assert_eq!(s.screen, Screen::List);
    }

    #[test]
    fn a_finger_on_the_chart_is_tracked_until_it_leaves() {
        let mut s = state(&["NVDA"]);
        assert!(!s.set_scrub(Some(10.0)), "the list has no chart to read");
        s.screen = Screen::Detail(Detail::new(0, Frame::Day));
        s.uptime_ms = 7_000;
        assert!(s.set_scrub(Some(10.0)));
        assert_eq!(s.touched_ms, 7_000, "a finger on the chart keeps it open");
        assert!(!s.set_scrub(Some(10.0)), "the same place changes nothing");
        assert!(s.set_scrub(Some(12.0)));
        assert!(s.set_scrub(None));
        assert!(matches!(&s.screen, Screen::Detail(d) if d.scrub.is_none()));
        assert!(!s.set_scrub(None));
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
}
