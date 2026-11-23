//! Live Markets for the Braiins Deck: prices that follow the market, with the pre-market and
//! after-hours moves of US stocks and ETFs, and a chart behind every symbol.
//!
//! One request per update asks CNBC's public quote service for the whole watchlist, every second
//! or two while a market is open and rarely while all are closed. A tap on a symbol opens its
//! chart, fetched from CNBC's chart service for the time frame chosen. The widget polls only
//! while it is on screen. The screens are built by `view`, from the state in `state`; `quote`
//! decides which price and move each session shows, and `chart` turns bars into a picture. Only
//! the glue below touches the host.

// The glue that uses most of the crate builds for the Deck only.
#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

mod chart;
mod format;
#[cfg(feature = "preview")]
mod preview;
mod quote;
mod state;
mod symbols;
mod view;

#[cfg(target_arch = "wasm32")]
mod widget {
    use std::cell::{Cell, RefCell};
    use std::time::Duration;

    use bmc_wasm_sdk::params::typed::ParamRead;
    use bmc_wasm_sdk::*;

    use crate::chart::{self, MAX_POINTS, RawBar, Series};
    use crate::quote::{Raw, number};
    use crate::state::State;
    use crate::symbols::{Entry, Kind, resolve_all};
    use crate::view;

    const QUOTE_URL: &str = "https://quote.cnbc.com/quote-html-webservice/restQuote/symbolType/symbol";
    const QUOTE_OPTIONS: &str = "requestMethod=itv&noform=1&partnerId=2&exthrs=1&output=json";
    /// The services refuse clients that do not say what they are, such as curl.
    const HEADERS: &str = "User-Agent: Mozilla/5.0 (X11; Linux armv7l) LiveMarkets/0.1\nAccept: application/json";
    const BASE: &str = "/FormattedQuoteResult/FormattedQuote/";
    const BARS: &str = "/barData/priceBars/";

    const DEFAULT_REFRESH_S: i32 = 2;
    /// While every symbol is closed the quotes only change at the next session: look rarely.
    const CLOSED_REFRESH_MS: u32 = 30_000;

    thread_local! {
        static STATE: RefCell<State> = RefCell::new(State::default());
        static POLL: RefCell<Option<PollHandle>> = const { RefCell::new(None) };
        /// The chart's poll: on only while a chart is open.
        static CHART: RefCell<Option<PollHandle>> = const { RefCell::new(None) };
        /// The cadence the operator asked for, while a market is open.
        static REFRESH_MS: Cell<u32> = const { Cell::new(2_000) };
    }

    fn read_params() -> (Vec<Entry>, u32) {
        let params = params::current();
        let list = <Vec<String> as ParamRead>::read_required(&params, "symbols");
        let seconds = <i32 as ParamRead>::read_optional(&params, "refresh_seconds").unwrap_or(DEFAULT_REFRESH_S);
        let refresh = u32::try_from(seconds.clamp(1, 60)).unwrap_or(2) * 1_000;
        (resolve_all(&list), refresh)
    }

    // ── Quotes ──────────────────────────────────────────────────────

    fn build_request(_handle: &PollHandle) -> Option<FetchSpec> {
        let list = STATE.with_borrow(State::request_list);
        if list.is_empty() {
            return None;
        }
        let url = fmt!("{}?symbols={}&{}", QUOTE_URL, list, QUOTE_OPTIONS);
        Some(FetchSpec::get(url).headers(HEADERS).timeout(Duration::from_secs(5)))
    }

    fn field(doc: &JsonDoc, base: &str, key: &str) -> String {
        doc.str(&fmt!("{}{}", base, key)).unwrap_or_default()
    }

    /// The service's reply, one `Raw` per symbol in it.
    fn raws(doc: &JsonDoc) -> Vec<Raw> {
        let count = doc.len("/FormattedQuoteResult/FormattedQuote").unwrap_or(0).min(32);
        (0..count)
            .map(|i| {
                let base = fmt!("{}{}/", BASE, i);
                let ext = fmt!("{}ExtendedMktQuote/", base);
                Raw {
                    symbol: field(doc, &base, "symbol"),
                    code: doc.i64(&fmt!("{}code", base)).unwrap_or(1),
                    name: field(doc, &base, "name"),
                    last: field(doc, &base, "last"),
                    prev_close: field(doc, &base, "previous_day_closing"),
                    change_pct: field(doc, &base, "change_pct"),
                    status: field(doc, &base, "curmktstatus"),
                    real_time: field(doc, &base, "realTime"),
                    last_time: field(doc, &base, "last_time"),
                    ext_type: field(doc, &ext, "type"),
                    ext_last: field(doc, &ext, "last"),
                    ext_change_pct: field(doc, &ext, "change_pct"),
                    ext_time: field(doc, &ext, "last_time"),
                }
            })
            .collect()
    }

    /// The next poll comes at the operator's cadence while anything trades, else rarely.
    fn retune(handle: &PollHandle) {
        let wanted = STATE.with_borrow(|s| if s.any_trading() { REFRESH_MS.get() } else { REFRESH_MS.get().max(CLOSED_REFRESH_MS) });
        let changed = STATE.with_borrow_mut(|s| std::mem::replace(&mut s.interval_ms, wanted) != wanted);
        if changed {
            handle.set_interval(wanted);
        }
    }

    fn on_reply(handle: &PollHandle, response: &FetchResponse) {
        let replies = if response.ok() {
            let doc = response.json();
            if doc.is_valid() { raws(&doc) } else { Vec::new() }
        } else {
            Vec::new()
        };
        if replies.is_empty() {
            log_warn!("live-markets: no usable reply (status {})", response.status);
            handle.retry();
        } else {
            STATE.with_borrow_mut(|s| s.apply(&replies, SystemTime::now().unix_secs));
            retune(handle);
        }
        request_frame();
    }

    // ── Charts ──────────────────────────────────────────────────────

    fn build_chart_request(_handle: &PollHandle) -> Option<FetchSpec> {
        let (query, _, frame) = STATE.with_borrow(State::chart_target)?;
        let url = chart::url(&query, frame);
        STATE.with_borrow_mut(|s| s.requested = Some((query, frame)));
        Some(FetchSpec::get(url).headers(HEADERS).timeout(Duration::from_secs(20)))
    }

    /// The bars of a reply, at most `MAX_POINTS` of them, spread over the frame.
    fn read_series(doc: &JsonDoc, kind: Kind, frame: chart::Frame) -> Option<Series> {
        let count = doc.len("/barData/priceBars")?;
        if count < 2 {
            return None;
        }
        let time_of = |i: usize| doc.i64(&fmt!("{}{}/tradeTimeinMills", BARS, i)).unwrap_or(0);
        let start = chart::start_index(count, time_of, frame.keep_secs(kind));
        let bars: Vec<RawBar> = chart::pick(start, count, MAX_POINTS)
            .into_iter()
            .filter_map(|i| {
                let base = fmt!("{}{}/", BARS, i);
                let ms = doc.i64(&fmt!("{}tradeTimeinMills", base))?;
                let close = doc.str(&fmt!("{}close", base)).as_deref().and_then(number).or_else(|| doc.f64(&fmt!("{}close", base)))?;
                let local = chart::trade_time(&doc.str(&fmt!("{}tradeTime", base))?)?;
                Some(RawBar { ms, local, close })
            })
            .collect();
        (bars.len() >= 2).then(|| Series::from_bars(frame, kind, &bars))
    }

    fn on_chart_reply(handle: &PollHandle, response: &FetchResponse) {
        let Some((query, frame)) = STATE.with_borrow_mut(|s| s.requested.take()) else { return };
        let kind = STATE.with_borrow(|s| s.chart_target().map(|t| t.1)).unwrap_or(Kind::Stock);
        let doc = response.ok().then(|| response.json()).filter(JsonDoc::is_valid);
        if let Some(series) = doc.and_then(|d| read_series(&d, kind, frame)) {
            STATE.with_borrow_mut(|s| s.accept_chart(&query, series));
        } else {
            log_warn!("live-markets: no usable chart (status {})", response.status);
            STATE.with_borrow_mut(|s| s.fail_chart(&query, frame));
            handle.retry();
        }
        request_frame();
    }

    /// Starts, retargets or stops the chart's poll to match the screen.
    fn sync_chart() {
        let target = STATE.with_borrow(State::chart_target);
        CHART.with_borrow(|handle| {
            let Some(handle) = handle else { return };
            match target {
                Some((_, _, frame)) => {
                    handle.set_interval(frame.refresh_ms());
                    handle.set_enabled(true);
                    handle.invalidate();
                }
                None => handle.set_enabled(false),
            }
        });
    }

    // ── The widget's hooks ──────────────────────────────────────────

    #[unsafe(no_mangle)]
    pub extern "C" fn init() {
        let (entries, refresh) = read_params();
        REFRESH_MS.set(refresh);
        STATE.with_borrow_mut(|s| {
            s.set_entries(entries);
            s.interval_ms = refresh;
        });
        #[cfg(feature = "preview")]
        let live = !STATE.with_borrow_mut(|s| crate::preview::seed(s, SystemTime::now().unix_secs));
        #[cfg(not(feature = "preview"))]
        let live = true;
        let handle = register_poll(
            build_request,
            on_reply,
            PollConfig {
                interval_ms: Some(refresh),
                retry_ms: 3_000,
                enabled: live,
                ..PollConfig::default()
            },
        );
        POLL.set(Some(handle));
        let chart_handle = register_poll(
            build_chart_request,
            on_chart_reply,
            PollConfig {
                interval_ms: Some(30_000),
                retry_ms: 5_000,
                enabled: false,
                ..PollConfig::default()
            },
        );
        CHART.set(Some(chart_handle));
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn on_params_update() {
        let (entries, refresh) = read_params();
        REFRESH_MS.set(refresh);
        STATE.with_borrow_mut(|s| {
            s.set_entries(entries);
            s.interval_ms = refresh;
        });
        POLL.with_borrow(|handle| {
            if let Some(handle) = handle {
                handle.set_interval(refresh);
                handle.invalidate();
            }
        });
        sync_chart();
        request_frame();
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn on_system_update() {
        request_frame();
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn on_touch() {
        request_frame();
    }

    /// Off screen nobody sees the prices or a chart: stop asking for them.
    #[unsafe(no_mangle)]
    pub extern "C" fn on_sleep() {
        STATE.with_borrow_mut(|s| {
            s.screen = crate::state::Screen::List;
        });
        for poll in [&POLL, &CHART] {
            poll.with_borrow(|handle| {
                if let Some(handle) = handle {
                    handle.set_enabled(false);
                }
            });
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn on_wake() {
        #[cfg(feature = "preview")]
        if STATE.with_borrow(|s| s.last_ok > 4_000_000_000) {
            return;
        }
        POLL.with_borrow(|handle| {
            if let Some(handle) = handle {
                handle.set_enabled(true);
                handle.invalidate();
            }
        });
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn render(delta_ms: u32) {
        let vp = widget_viewport();
        let now = SystemTime::now().unix_secs;
        let (closed, tree, hits, animating) = STATE.with_borrow_mut(|s| {
            s.uptime_ms += u64::from(delta_ms.min(5_000));
            let closed = s.close_if_idle();
            let (tree, hits) = view::screen(s, vp.width, vp.height, now);
            (closed, tree, hits, s.flashing())
        });
        let result = render_ui(vp.width, vp.height, tree);
        // The taps came in on the screen drawn before this one, so they are read against its
        // buttons, and only then are the new screen's buttons kept for the next taps.
        let tapped = STATE.with_borrow_mut(|s| {
            let changed = result.clicks.get("tap").is_some_and(|hit| s.on_tap(hit.x, hit.y));
            s.hits = Some(hits);
            changed
        });
        if closed || tapped {
            sync_chart();
        }
        if tapped {
            request_frame();
        }
        // The clock behind page turns, flashes and the freshness light.
        request_frame_after(if animating { 300 } else { 1_000 });
    }
}
