//! Live Markets for the Braiins Deck: prices that follow the market, with a chart behind every symbol.
//!
//! Prices come from Hyperliquid by default: its perpetuals trade round the clock, crypto and the stocks,
//! indexes and commodities that builders list there, and the prices are pushed over one WebSocket.
//! What Hyperliquid has no market for, such as the US stocks of a few names and the Dow, comes from
//! CNBC's public quote service instead, one request per update for those symbols, with the
//! pre-market and after-hours moves of US stocks. A tap on a symbol opens its chart, taken from the
//! same place as its price. The widget asks for nothing while it is off screen. The screens are
//! built by `view`, from the state in `state`; `quote` decides which price and move each session
//! shows, `hl` speaks Hyperliquid's protocol and `chart` turns bars into a picture. Only the glue
//! below touches the host.

// The glue that uses most of the crate builds for the Deck only.
#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

mod chart;
mod clock;
mod format;
mod hl;
#[cfg(feature = "preview")]
mod preview;
mod quote;
mod state;
mod symbols;
mod tv;
mod view;
mod ws;

#[cfg(target_arch = "wasm32")]
mod widget {
    use std::cell::{Cell, RefCell};
    use std::time::Duration;

    use bmc_wasm_sdk::params::typed::ParamRead;
    use bmc_wasm_sdk::socket::{Socket, SocketEvent, tls_connect};
    use bmc_wasm_sdk::*;

    use crate::chart::{self, MAX_POINTS, RawBar, Series};
    use crate::hl;
    use crate::quote::{Raw, number};
    use crate::state::State;
    use crate::symbols::{Entry, Kind, Source, resolve_all};
    use crate::tv;
    use crate::view::{self, Hits};
    use crate::ws;

    const QUOTE_URL: &str = "https://quote.cnbc.com/quote-html-webservice/restQuote/symbolType/symbol";
    const QUOTE_OPTIONS: &str = "requestMethod=itv&noform=1&partnerId=2&exthrs=1&output=json";
    /// The services refuse clients that do not say what they are, such as curl.
    const HEADERS: &str = "User-Agent: Mozilla/5.0 (X11; Linux armv7l) LiveMarkets/0.4\nAccept: application/json";
    /// What Hyperliquid's info endpoint wants of a request.
    const POST_HEADERS: &str = "User-Agent: Mozilla/5.0 (X11; Linux armv7l) LiveMarkets/0.4\nAccept: application/json\nContent-Type: application/json";
    const BASE: &str = "/FormattedQuoteResult/FormattedQuote/";
    const BARS: &str = "/barData/priceBars/";

    /// What the scanner of TradingView wants of a request.
    const TV_HEADERS: &str = "User-Agent: Mozilla/5.0 (X11; Linux armv7l) LiveMarkets/0.5\nAccept: application/json\nContent-Type: application/json\nOrigin: https://www.tradingview.com";
    /// How often TradingView is asked while it prices something that moves (the VIX), and when it prices only
    /// indicators, which are released once a month.
    const TV_LIVE_MS: u32 = 10_000;
    const TV_RELEASE_MS: u32 = 600_000;

    const DEFAULT_REFRESH_S: i32 = 2;
    /// While every symbol of CNBC's is closed the quotes only change at the next session: look rarely.
    const CLOSED_REFRESH_MS: u32 = 30_000;
    /// The first wait before the socket is opened again after it failed; it doubles up to `MAX_BACKOFF_MS`.
    const FIRST_BACKOFF_MS: u64 = 3_000;
    const MAX_BACKOFF_MS: u64 = 30_000;

    thread_local! {
        static STATE: RefCell<State> = RefCell::new(State::default());
        /// CNBC's poll: on only while it has a symbol to price.
        static POLL: RefCell<Option<PollHandle>> = const { RefCell::new(None) };
        /// The chart's poll: on only while a chart is open.
        static CHART: RefCell<Option<PollHandle>> = const { RefCell::new(None) };
        /// TradingView's poll: on only while it has symbols to price.
        static TV_POLL: RefCell<Option<PollHandle>> = const { RefCell::new(None) };
        static TV_EVERY_MS: Cell<u32> = const { Cell::new(TV_LIVE_MS) };
        /// The chart socket of TradingView, and the chart it is being asked for.
        static TV_SOCKET: RefCell<Option<Socket>> = const { RefCell::new(None) };
        static TV_DECODER: RefCell<ws::Decoder> = RefCell::new(ws::Decoder::default());
        static TV_JOB: RefCell<Option<TvJob>> = const { RefCell::new(None) };
        /// The cadence the operator asked for, while a market is open.
        static REFRESH_MS: Cell<u32> = const { Cell::new(2_000) };
        /// The connection to Hyperliquid: a TLS socket that speaks WebSocket (see `ws`).
        static SOCKET: RefCell<Option<Socket>> = const { RefCell::new(None) };
        static DECODER: RefCell<ws::Decoder> = RefCell::new(ws::Decoder::default());
        /// The state of the generator of the keys and masks of the frames sent.
        static RANDOM: Cell<u64> = const { Cell::new(0x9E37_79B9_7F4A_7C15) };
        /// The WebSocket has opened, so a close is a drop and not a failure to connect.
        static OPENED: Cell<bool> = const { Cell::new(false) };
        /// `uptime_ms` before which the socket is not opened again, and how long that wait grows to.
        static RETRY_AT: Cell<u64> = const { Cell::new(0) };
        static BACKOFF_MS: Cell<u64> = const { Cell::new(FIRST_BACKOFF_MS) };
        static PINGED_AT: Cell<u64> = const { Cell::new(0) };
        /// False when the screens show sample data, which no feed may replace.
        static LIVE: Cell<bool> = const { Cell::new(true) };
        /// False while the widget is off screen.
        static AWAKE: Cell<bool> = const { Cell::new(true) };
    }

    /// A chart being fetched from TradingView's chart socket: whose, in which time frame, and the bars so far.
    struct TvJob {
        query: String,
        kind: Kind,
        frame: chart::Frame,
        ticker: String,
        bars: Vec<(i64, f64)>,
        started_ms: u64,
    }

    fn read_params() -> (Vec<Entry>, u32, Source) {
        let params = params::current();
        let list = <Vec<String> as ParamRead>::read_required(&params, "symbols");
        let seconds = <i32 as ParamRead>::read_optional(&params, "refresh_seconds").unwrap_or(DEFAULT_REFRESH_S);
        let source = Source::from_param(<String as ParamRead>::read_optional(&params, "source").as_deref());
        let refresh = u32::try_from(seconds.clamp(1, 60)).unwrap_or(2) * 1_000;
        (resolve_all(&list), refresh, source)
    }

    // ── Quotes from CNBC ────────────────────────────────────────────

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

    /// Switches the polls of CNBC and TradingView on while they have symbols to price and off while they have
    /// none; `refetch` asks for the prices at once, as after a symbol was handed over to CNBC.
    fn sync_quotes(refetch: bool) {
        let awake = LIVE.get() && AWAKE.get();
        let (cnbc, tv) = STATE.with_borrow(|s| (s.needs_cnbc(), s.needs_tv()));
        for (poll, want) in [(&POLL, awake && cnbc), (&TV_POLL, awake && tv)] {
            poll.with_borrow(|handle| {
                let Some(handle) = handle else { return };
                handle.set_enabled(want);
                if want && refetch {
                    handle.invalidate();
                }
            });
        }
        retune_tv();
    }

    // ── Indicators and indexes from TradingView ─────────────────────

    fn build_tv_request(_handle: &PollHandle) -> Option<FetchSpec> {
        let body = tv::scan_body(&STATE.with_borrow(State::tv_tickers))?;
        Some(FetchSpec::post(tv::SCAN_URL).headers(TV_HEADERS).body(body).timeout(Duration::from_secs(10)))
    }

    /// The scanner is asked often while the VIX or another index is on the list, and rarely when there are only
    /// indicators, which are released once a month.
    fn retune_tv() {
        let wanted = if STATE.with_borrow(State::tv_trades) { TV_LIVE_MS } else { TV_RELEASE_MS };
        if TV_EVERY_MS.replace(wanted) != wanted {
            TV_POLL.with_borrow(|handle| {
                if let Some(handle) = handle {
                    handle.set_interval(wanted);
                }
            });
        }
    }

    fn on_tv_reply(handle: &PollHandle, response: &FetchResponse) {
        let answer = response.ok().then(|| response.text()).flatten().filter(|text| text.contains(r#""data":"#));
        if let Some(text) = answer {
            let readings = tv::readings(text);
            STATE.with_borrow_mut(|s| s.apply_tv(&readings, SystemTime::now().unix_secs));
        } else {
            log_warn!("live-markets: no usable answer from tradingview (status {})", response.status);
            handle.retry();
        }
        request_frame();
    }

    /// Opens TradingView's chart socket for a symbol's bars, unless it is already doing so for this chart.
    fn start_tv_chart(query: String, kind: Kind, frame: chart::Frame, ticker: String) {
        if TV_JOB.with_borrow(|job| job.as_ref().is_some_and(|j| j.query == query && j.frame == frame)) {
            return;
        }
        TV_SOCKET.set(None);
        let started_ms = STATE.with_borrow(|s| s.uptime_ms);
        if let Some(socket) = tls_connect(tv::HOST, 443, on_tv_event) {
            TV_SOCKET.set(Some(socket));
            TV_JOB.set(Some(TvJob { query, kind, frame, ticker, bars: Vec::new(), started_ms }));
        } else {
            log_warn!("live-markets: the host would not open the chart socket");
            STATE.with_borrow_mut(|s| s.fail_chart(&query, frame));
        }
    }

    /// The chart is in, or could not be had: the socket is closed and the screen told.
    fn finish_tv_job(complete: bool) {
        let Some(job) = TV_JOB.take() else { return };
        TV_SOCKET.set(None);
        let series = if complete { tv::series(&job.bars, job.kind, job.frame) } else { None };
        if let Some(series) = series {
            STATE.with_borrow_mut(|s| s.accept_chart(&job.query, series));
        } else {
            log_warn!("live-markets: no usable chart from tradingview ({} bars)", job.bars.len());
            STATE.with_borrow_mut(|s| s.fail_chart(&job.query, job.frame));
            CHART.with_borrow(|handle| {
                if let Some(handle) = handle {
                    handle.retry();
                }
            });
        }
        request_frame();
    }

    /// The upgrade was accepted: ask for the bars.
    fn ask_tv_chart(socket: &Socket) {
        let Some((kind, frame, ticker)) = TV_JOB.with_borrow(|job| job.as_ref().map(|j| (j.kind, j.frame, j.ticker.clone()))) else { return };
        let mut state = RANDOM.get();
        let id: String = (0..12).map(|_| char::from(b'a' + u8::try_from(ws::next_random(&mut state) % 26).unwrap_or(0))).collect();
        RANDOM.set(state);
        let (interval, count) = tv::chart_request(kind, frame);
        for message in tv::chart_messages(&format!("cs_{id}"), &ticker, interval, count) {
            send_text(socket, &message);
        }
    }

    /// What the chart socket says. Whether the chart is finished.
    fn on_tv_text(socket: &Socket, text: &str) -> bool {
        for payload in tv::split_messages(text) {
            if payload.starts_with("~h~") {
                send_text(socket, &tv::frame_message(payload));
                continue;
            }
            match tv::chart_event(payload) {
                tv::ChartEvent::Bars(bars) => TV_JOB.with_borrow_mut(|job| {
                    if let Some(job) = job {
                        job.bars.extend(bars);
                    }
                }),
                tv::ChartEvent::Completed => {
                    finish_tv_job(true);
                    return true;
                }
                tv::ChartEvent::Failed => {
                    finish_tv_job(false);
                    return true;
                }
                tv::ChartEvent::Other => {}
            }
        }
        false
    }

    fn on_tv_event(socket: &Socket, event: &SocketEvent<'_>) {
        match event {
            SocketEvent::Connected => {
                TV_DECODER.set(ws::Decoder::default());
                let mut state = RANDOM.get();
                let key = ws::key(&mut state);
                RANDOM.set(state);
                socket.write(ws::request(tv::HOST, tv::WS_PATH, &key, &[("Origin", tv::ORIGIN)]).as_bytes());
            }
            SocketEvent::Data(bytes) => {
                for event in TV_DECODER.with_borrow_mut(|decoder| decoder.push(bytes)) {
                    match event {
                        ws::Event::Open => ask_tv_chart(socket),
                        ws::Event::Text(text) => {
                            if on_tv_text(socket, &text) {
                                return;
                            }
                        }
                        ws::Event::Ping(payload) => {
                            let mut state = RANDOM.get();
                            let mask = ws::next_random(&mut state).to_le_bytes();
                            RANDOM.set(state);
                            socket.write(&ws::encode(ws::PONG, &payload, [mask[0], mask[1], mask[2], mask[3]]));
                        }
                        ws::Event::Close | ws::Event::Refused => {
                            finish_tv_job(false);
                            return;
                        }
                    }
                }
            }
            SocketEvent::Closed(_) => finish_tv_job(false),
        }
    }

    // ── Prices from Hyperliquid ─────────────────────────────────────

    /// Sends a text message down the WebSocket.
    fn send_text(socket: &Socket, text: &str) {
        let mut state = RANDOM.get();
        let mask = ws::next_random(&mut state).to_le_bytes();
        RANDOM.set(state);
        socket.write(&ws::encode(ws::TEXT, text.as_bytes(), [mask[0], mask[1], mask[2], mask[3]]));
    }

    fn on_ws_message(text: &str) {
        let now = SystemTime::now().unix_secs;
        let handed_over = STATE.with_borrow_mut(|s| {
            s.hl_heard();
            match hl::parse(text) {
                hl::Message::Ctx { coin, ctx } => {
                    s.apply_hl(&coin, &ctx, now);
                    false
                }
                hl::Message::Refused(coin) => {
                    log_info!("live-markets: hyperliquid has no market {}", coin);
                    s.refuse_hl(&coin)
                }
                hl::Message::Other => false,
            }
        });
        if handed_over {
            log_info!("live-markets: a symbol goes to CNBC");
            sync_quotes(true);
        }
        request_frame();
    }

    /// The upgrade was accepted: ask for the prices of every market the symbols may have.
    fn on_ws_open(socket: &Socket) {
        OPENED.set(true);
        BACKOFF_MS.set(FIRST_BACKOFF_MS);
        let coins = STATE.with_borrow_mut(|s| {
            s.hl_expect();
            s.hl_coins()
        });
        log_info!("live-markets: hyperliquid is open, asking for {} markets", coins.len());
        for coin in coins {
            if let Some(message) = hl::subscribe(&coin) {
                send_text(socket, &message);
            }
        }
        // The symbols that were handed to CNBC while this was down may be asked of it no longer.
        sync_quotes(false);
    }

    /// The connection is gone, or was never made: try again after a wait, and until it is back let CNBC
    /// price what has no price on screen yet.
    fn on_ws_closed(code: u32) {
        SOCKET.set(None);
        let now = STATE.with_borrow(|s| s.uptime_ms);
        let failed = !OPENED.get();
        if failed {
            let wait = BACKOFF_MS.get();
            RETRY_AT.set(now + wait);
            BACKOFF_MS.set((wait * 2).min(MAX_BACKOFF_MS));
        } else {
            // It was up and dropped: try again soon.
            RETRY_AT.set(now + 2_000);
        }
        OPENED.set(false);
        log_warn!("live-markets: hyperliquid connection closed (code {}, opened {})", code, !failed);
        if failed && STATE.with_borrow_mut(State::hl_failed) {
            sync_quotes(true);
        }
        request_frame();
    }

    fn on_socket_event(socket: &Socket, event: &SocketEvent<'_>) {
        match event {
            SocketEvent::Connected => {
                DECODER.set(ws::Decoder::default());
                let mut state = RANDOM.get();
                let key = ws::key(&mut state);
                RANDOM.set(state);
                socket.write(ws::request(hl::HOST, hl::WS_PATH, &key, &[]).as_bytes());
            }
            SocketEvent::Data(bytes) => {
                for event in DECODER.with_borrow_mut(|decoder| decoder.push(bytes)) {
                    match event {
                        ws::Event::Open => on_ws_open(socket),
                        ws::Event::Text(text) => on_ws_message(&text),
                        ws::Event::Ping(payload) => {
                            let mut state = RANDOM.get();
                            let mask = ws::next_random(&mut state).to_le_bytes();
                            RANDOM.set(state);
                            socket.write(&ws::encode(ws::PONG, &payload, [mask[0], mask[1], mask[2], mask[3]]));
                        }
                        ws::Event::Close | ws::Event::Refused => {
                            // Ours to close; no `Closed` event follows, so the end is handled here.
                            on_ws_closed(1);
                            return;
                        }
                    }
                }
            }
            SocketEvent::Closed(code) => on_ws_closed(*code),
        }
    }

    /// Opens the connection when there are symbols for it and it is not open, and keeps it alive. It is
    /// looked at on every frame, and is closed when there is nothing to ask of it.
    fn ensure_ws() {
        if !LIVE.get() || !AWAKE.get() {
            return;
        }
        let (wants, now) = STATE.with_borrow(|s| (s.wants_hl(), s.uptime_ms));
        if !wants {
            SOCKET.set(None);
            return;
        }
        if SOCKET.with_borrow(Option::is_some) {
            if OPENED.get() && now.saturating_sub(PINGED_AT.get()) >= hl::PING_EVERY_MS {
                PINGED_AT.set(now);
                SOCKET.with_borrow(|socket| {
                    if let Some(socket) = socket {
                        send_text(socket, hl::PING);
                    }
                });
            }
            return;
        }
        if now < RETRY_AT.get() {
            return;
        }
        OPENED.set(false);
        PINGED_AT.set(now);
        RETRY_AT.set(now + BACKOFF_MS.get());
        if let Some(socket) = tls_connect(hl::HOST, 443, on_socket_event) {
            SOCKET.set(Some(socket));
        } else {
            log_warn!("live-markets: the host would not open the connection");
            BACKOFF_MS.set((BACKOFF_MS.get() * 2).min(MAX_BACKOFF_MS));
            if STATE.with_borrow_mut(State::hl_failed) {
                sync_quotes(true);
            }
        }
    }

    /// Drops the connection, so that the next `ensure_ws` opens a fresh one with the symbols as they are now.
    fn reset_ws() {
        SOCKET.set(None);
        OPENED.set(false);
        RETRY_AT.set(0);
        BACKOFF_MS.set(FIRST_BACKOFF_MS);
    }

    // ── Charts ──────────────────────────────────────────────────────

    fn build_chart_request(_handle: &PollHandle) -> Option<FetchSpec> {
        let (query, kind, frame) = STATE.with_borrow(State::chart_target)?;
        if let Some(ticker) = STATE.with_borrow(State::chart_tv) {
            // TradingView's bars come over its socket, not from a request: start it, and no fetch follows.
            start_tv_chart(query, kind, frame, ticker);
            return None;
        }
        let spec = if kind == Kind::Perp {
            let coin = STATE.with_borrow(State::chart_coin)?;
            let body = hl::candle_body(&coin, frame, SystemTime::now().unix_secs * 1_000)?;
            FetchSpec::post(hl::INFO_URL).headers(POST_HEADERS).body(body).timeout(Duration::from_secs(15))
        } else {
            FetchSpec::get(chart::url(&query, frame)).headers(HEADERS).timeout(Duration::from_secs(20))
        };
        STATE.with_borrow_mut(|s| s.requested = Some((query, kind, frame)));
        Some(spec)
    }

    /// The bars of a reply from CNBC, at most `MAX_POINTS` of them, spread over the frame.
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

    /// The candles of a reply from Hyperliquid, at most `MAX_POINTS` of them, each with the time on the
    /// Deck's own clock: a perpetual has no exchange hours to show the time of.
    fn read_candles(text: &str, frame: chart::Frame) -> Option<Series> {
        let all = hl::candles(text);
        if all.len() < 2 {
            return None;
        }
        let now = SystemTime::now();
        let offset = bmc_wasm_sdk::format::local_unix_secs_or_system(&now, None) - now.unix_secs;
        let bars: Vec<RawBar> = chart::pick(0, all.len(), MAX_POINTS)
            .into_iter()
            .filter_map(|i| all.get(i))
            .map(|&(ms, close)| RawBar { ms, local: ms / 1_000 + offset, close })
            .collect();
        Some(Series::from_bars(frame, Kind::Perp, &bars))
    }

    fn on_chart_reply(handle: &PollHandle, response: &FetchResponse) {
        let Some((query, kind, frame)) = STATE.with_borrow_mut(|s| s.requested.take()) else { return };
        let series = if !response.ok() {
            None
        } else if kind == Kind::Perp {
            response.text().and_then(|text| read_candles(text, frame))
        } else {
            Some(response.json()).filter(JsonDoc::is_valid).and_then(|doc| read_series(&doc, kind, frame))
        };
        if let Some(series) = series {
            STATE.with_borrow_mut(|s| s.accept_chart(&query, series));
        } else {
            // What came back, so that a reply the Deck cannot read can be seen in its log.
            let head: String = response.text().unwrap_or_default().chars().take(80).collect();
            log_warn!("live-markets: no usable chart (status {}, {} bytes): {}", response.status, response.body().len(), head);
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
                // The sample screens keep their own bars: nothing is fetched for them.
                Some((_, _, frame)) if LIVE.get() => {
                    handle.set_interval(frame.refresh_ms());
                    handle.set_enabled(AWAKE.get());
                    handle.invalidate();
                }
                _ => handle.set_enabled(false),
            }
        });
    }

    // ── The widget's hooks ──────────────────────────────────────────

    #[unsafe(no_mangle)]
    pub extern "C" fn init() {
        let (entries, refresh, source) = read_params();
        REFRESH_MS.set(refresh);
        STATE.with_borrow_mut(|s| {
            s.set_entries(entries, source);
            s.interval_ms = refresh;
        });
        #[cfg(feature = "preview")]
        let live = !STATE.with_borrow_mut(|s| crate::preview::seed(s, SystemTime::now().unix_secs));
        #[cfg(not(feature = "preview"))]
        let live = true;
        LIVE.set(live);
        let handle = register_poll(
            build_request,
            on_reply,
            PollConfig {
                interval_ms: Some(refresh),
                retry_ms: 3_000,
                enabled: live && STATE.with_borrow(State::needs_cnbc),
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
        let tv_handle = register_poll(
            build_tv_request,
            on_tv_reply,
            PollConfig {
                interval_ms: Some(TV_LIVE_MS),
                retry_ms: 5_000,
                enabled: live && STATE.with_borrow(State::needs_tv),
                ..PollConfig::default()
            },
        );
        TV_POLL.set(Some(tv_handle));
        retune_tv();
        sync_chart();
        ensure_ws();
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn on_params_update() {
        let (entries, refresh, source) = read_params();
        REFRESH_MS.set(refresh);
        let before = STATE.with_borrow(State::hl_coins);
        STATE.with_borrow_mut(|s| {
            s.set_entries(entries, source);
            s.interval_ms = refresh;
        });
        POLL.with_borrow(|handle| {
            if let Some(handle) = handle {
                handle.set_interval(refresh);
            }
        });
        sync_quotes(true);
        // A connection that was asked for these very markets does as it is.
        if STATE.with_borrow(State::hl_coins) != before {
            reset_ws();
        }
        ensure_ws();
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
        AWAKE.set(false);
        STATE.with_borrow_mut(|s| {
            s.screen = crate::state::Screen::List;
        });
        SOCKET.set(None);
        TV_JOB.set(None);
        TV_SOCKET.set(None);
        for poll in [&POLL, &CHART, &TV_POLL] {
            poll.with_borrow(|handle| {
                if let Some(handle) = handle {
                    handle.set_enabled(false);
                }
            });
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn on_wake() {
        AWAKE.set(true);
        if !LIVE.get() {
            return;
        }
        reset_ws();
        STATE.with_borrow_mut(State::hl_expect);
        sync_quotes(true);
        ensure_ws();
        request_frame();
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn render(delta_ms: u32) {
        let vp = widget_viewport();
        let now = SystemTime::now().unix_secs;
        let live = LIVE.get();
        let (closed, tree, hits, animating, handed_over) = STATE.with_borrow_mut(|s| {
            s.uptime_ms += u64::from(delta_ms.min(5_000));
            let handed_over = live && s.hl_watch();
            let closed = s.close_if_idle();
            s.expire_scrub();
            let (tree, hits) = view::screen(s, vp.width, vp.height, now);
            (closed, tree, hits, s.flashing(), handed_over)
        });
        if handed_over {
            sync_quotes(true);
        }
        ensure_ws();
        let stuck = TV_JOB.with_borrow(|job| job.as_ref().is_some_and(|j| STATE.with_borrow(|s| s.uptime_ms.saturating_sub(j.started_ms)) > tv::CHART_PATIENCE_MS));
        if stuck {
            finish_tv_job(false);
        }
        let result = render_ui(vp.width, vp.height, tree);
        // The taps came in on the screen drawn before this one, so they are read against its
        // buttons, and only then are the new screen's buttons kept for the next taps.
        let mut jogging = false;
        let tapped = STATE.with_borrow_mut(|s| {
            let changed = result.clicks.get("tap").is_some_and(|hit| s.on_tap(hit.x, hit.y));
            // A finger on the chart, or one that touched it for less than a frame: the cursor goes there.
            let touch = result.drags.get("scrub").or_else(|| result.clicks.get("scrub"));
            let scrubbed = s.set_scrub(touch.map(|hit| hit.x));
            // An arrow held, or tapped between two frames: the cursor moves along the chart.
            let plot_w = if let Hits::Detail(layout) = &hits { layout.plot_w() } else { 0.0 };
            let arrow = |map: &std::collections::HashMap<String, TouchHit>| {
                [("jog_l", -1_i8), ("jog_r", 1)].into_iter().find(|(key, _)| map.contains_key(*key)).map(|(_, dir)| dir)
            };
            let held = arrow(&result.drags);
            let jogged = match (held, arrow(&result.clicks)) {
                (Some(dir), _) => s.jog(Some(dir), u64::from(delta_ms.min(100)), plot_w),
                (None, Some(dir)) => s.jog(Some(dir), 0, plot_w) | s.jog(None, 0, plot_w),
                (None, None) => s.jog(None, 0, plot_w),
            };
            jogging = held.is_some();
            s.hits = Some(hits);
            changed || scrubbed || jogged
        });
        if closed || tapped {
            sync_chart();
        }
        if tapped {
            request_frame();
        }
        // The clock behind page turns, flashes and the freshness light; a held arrow wants every frame.
        request_frame_after(if jogging { 16 } else if animating { 300 } else { 1_000 });
    }
}
