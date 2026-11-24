//! The screens. Everything here builds a tree from the state and the size it is given, so it
//! runs on the host as well, for the tests and the previews.
//!
//! Each screen also says where its buttons are (`Hits`), so that a tap, which arrives as a
//! position, can be turned into what it pressed (`resolve`).

use bmc_wasm_sdk::format::{FormatTimeOpts, format_time};
use bmc_wasm_sdk::*;

use crate::chart::{self, Frame, Phase, Plot};
use crate::format;
use crate::quote::{Note, Quote, Session, Tag, View};
use crate::state::{Detail, FLASH_MS, Health, Row, Screen, Scrub, State};
use crate::symbols::Kind;

const BG: Color = Color::from_hex(0x0D_0F_13);
const CARD: Color = Color::from_hex(0x16_19_20);
const INK: Color = Color::from_hex(0xF3_F5_F8);
const MUTE: Color = Color::from_hex(0x8C_94_A3);
const DIM: Color = Color::from_hex(0x58_5F_6C);
const GRID: Color = Color::from_hex(0x23_27_30);
const UP: Color = Color::from_hex(0x2F_D1_7C);
const DOWN: Color = Color::from_hex(0xFF_5C_62);
const AMBER: Color = Color::from_hex(0xF6_A8_2B);
const VIOLET: Color = Color::from_hex(0xA2_93_FF);
const SKY: Color = Color::from_hex(0x4F_B7_F0);

const HEADER_H: f32 = 52.0;
const MIN_CELL_W: f32 = 270.0;
const MAX_CELL_W: f32 = 640.0;
/// A cell this tall has room for a second line.
const TALL_H: f32 = 76.0;
const MIN_CELL_H: f32 = 52.0;
const MAX_CELL_H: f32 = 120.0;
/// How long a page of symbols stays up before the next one.
pub const PAGE_MS: u64 = 8_000;
/// A fingertip is bigger than a button: taps this close to a button still press it.
const SLOP: f32 = 6.0;

// ── Where things are, and what a tap on them does ──────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    #[must_use]
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    #[must_use]
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x - SLOP && x < self.x + self.w + SLOP && y >= self.y - SLOP && y < self.y + self.h + SLOP
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetailClass {
    Full,
    Medium,
    Compact,
}

/// The buttons and the chart of a symbol's screen.
#[derive(Clone, Debug, PartialEq)]
pub struct DetailLayout {
    pub class: DetailClass,
    pub pad: f32,
    pub head_h: f32,
    pub back: Rect,
    pub prev: Option<Rect>,
    pub next: Option<Rect>,
    /// The button that lets the cursor go; there while the cursor is on the chart.
    pub live: Option<Rect>,
    /// Arrows that hold the cursor in motion along the chart, in place of a slide the Deck would take for a swipe.
    pub jog_left: Option<Rect>,
    pub jog_right: Option<Rect>,
    pub frames: [Rect; 6],
    /// The chart with its axis labels.
    pub chart: Rect,
    /// Where the symbol's name starts, and where the price block must end.
    pub title_x: f32,
    pub price_edge: f32,
}

/// Width of the Live button and of one jog arrow on the full screen.
const LIVE_W: f32 = 150.0;
const JOG_W: f32 = 64.0;

/// Room a chart leaves for its price labels (right) and its time labels (below).
const fn chart_margins(class: DetailClass) -> (f32, f32) {
    match class {
        DetailClass::Full => (76.0, 30.0),
        DetailClass::Medium => (66.0, 28.0),
        DetailClass::Compact => (50.0, 22.0),
    }
}

impl DetailLayout {
    /// Width of the plotting box inside the chart, the span of the cursor.
    #[must_use]
    pub fn plot_w(&self) -> f32 {
        self.chart.w - chart_margins(self.class).0
    }
}

#[must_use]
pub fn detail_layout(w: f32, h: f32) -> DetailLayout {
    let class = if h < 300.0 {
        DetailClass::Compact
    } else if w < 900.0 {
        DetailClass::Medium
    } else {
        DetailClass::Full
    };
    let (pad, head_h, back_w, nav, gap, btn_h, chart_gap) = match class {
        DetailClass::Full => (24.0, 56.0, 120.0, 52.0, 10.0, 48.0, 14.0),
        DetailClass::Medium => (16.0, 48.0, 100.0, 44.0, 8.0, 44.0, 10.0),
        DetailClass::Compact => (10.0, 38.0, 44.0, 0.0, 5.0, 32.0, 6.0),
    };
    let back = Rect::new(pad, pad, back_w, if class == DetailClass::Compact { 36.0 } else { head_h - 4.0 });
    let (prev, next) = if nav > 0.0 {
        let next = Rect::new(w - pad - nav, pad, nav, nav);
        (Some(Rect::new(next.x - 8.0 - nav, pad, nav, nav)), Some(next))
    } else {
        (None, None)
    };
    let btn_w = (w - 2.0 * pad - 5.0 * gap) / 6.0;
    let btn_w = if class == DetailClass::Full { btn_w.min(100.0) } else { btn_w };
    let frames_y = h - pad - btn_h;
    let frames = std::array::from_fn(|i| {
        #[expect(clippy::cast_precision_loss, reason = "an index below six")]
        let i = i as f32;
        Rect::new(pad + i * (btn_w + gap), frames_y, btn_w, btn_h)
    });
    let chart_y = pad + head_h + chart_gap;
    let chart = Rect::new(pad, chart_y, w - 2.0 * pad, (frames_y - chart_gap - chart_y).max(40.0));
    let (axis_w, label_h) = chart_margins(class);
    let (jog_left, jog_right) = match class {
        // In the row of the buttons, left of Live.
        DetailClass::Full => {
            let right = Rect::new(w - pad - LIVE_W - 10.0 - JOG_W, frames_y, JOG_W, btn_h);
            (Some(Rect::new(right.x - 10.0 - JOG_W, frames_y, JOG_W, btn_h)), Some(right))
        }
        // Over the foot of the chart, at its two ends.
        DetailClass::Medium => {
            let y = chart.y + chart.h - label_h - 8.0 - 40.0;
            (Some(Rect::new(chart.x + 8.0, y, 56.0, 40.0)), Some(Rect::new(chart.x + chart.w - axis_w - 8.0 - 56.0, y, 56.0, 40.0)))
        }
        DetailClass::Compact => (None, None),
    };
    DetailLayout {
        class,
        pad,
        head_h,
        back,
        prev,
        next,
        live: None,
        jog_left,
        jog_right,
        frames,
        chart,
        title_x: back.x + back.w + if class == DetailClass::Compact { 10.0 } else { 18.0 },
        price_edge: prev.map_or(w - pad, |p| p.x - 18.0),
    }
}

/// Where the buttons of the screen on show are.
#[derive(Clone, Debug, PartialEq)]
pub enum Hits {
    /// Each cell of the page, and the number of the first row it shows.
    List { cells: Vec<Rect>, first: usize },
    Detail(DetailLayout),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tap {
    Nothing,
    /// A symbol of the list, by its place in the watchlist.
    Open(usize),
    Back,
    Frame(Frame),
    /// The previous (-1) or next (1) symbol.
    Step(isize),
    /// Back to the live price, from the price the cursor reads.
    Live,
}

#[must_use]
pub fn resolve(hits: &Hits, x: f32, y: f32) -> Tap {
    match hits {
        Hits::List { cells, first } => cells.iter().position(|c| c.contains(x, y)).map_or(Tap::Nothing, |i| Tap::Open(first + i)),
        Hits::Detail(d) => {
            if d.live.is_some_and(|r| r.contains(x, y)) {
                Tap::Live
            } else if d.back.contains(x, y) {
                Tap::Back
            } else if d.prev.is_some_and(|r| r.contains(x, y)) {
                Tap::Step(-1)
            } else if d.next.is_some_and(|r| r.contains(x, y)) {
                Tap::Step(1)
            } else {
                d.frames.iter().position(|r| r.contains(x, y)).map_or(Tap::Nothing, |i| Tap::Frame(Frame::ALL[i]))
            }
        }
    }
}

// ── The list ───────────────────────────────────────────────────────

/// How the list is cut up for a size and a number of symbols.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plan {
    pub header: bool,
    pub pad: f32,
    pub gap: f32,
    pub cols: usize,
    pub rows: usize,
    pub cell_w: f32,
    pub cell_h: f32,
}

impl Plan {
    /// Symbols on one page; the rest take turns.
    #[must_use]
    pub fn per_page(&self) -> usize {
        self.cols * self.rows
    }

    /// Where the `k`th cell of a page is.
    #[must_use]
    #[expect(clippy::cast_precision_loss, reason = "a few cells")]
    pub fn cell(&self, k: usize) -> Rect {
        let (col, row) = (k % self.cols, k / self.cols);
        let top = self.pad + if self.header { HEADER_H + self.gap } else { 0.0 };
        Rect::new(
            self.pad + col as f32 * (self.cell_w + self.gap),
            top + row as f32 * (self.cell_h + self.gap),
            self.cell_w,
            self.cell_h,
        )
    }
}

/// The widest layout that still leaves each cell room for a second line, else as many rows as fit.
#[must_use]
#[expect(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a few cells")]
pub fn plan(w: f32, h: f32, n: usize) -> Plan {
    let n = n.max(1);
    let header = h >= 300.0;
    let pad = if w >= 1000.0 {
        24.0
    } else if w >= 600.0 {
        16.0
    } else {
        10.0
    };
    let gap = if w >= 600.0 { 12.0 } else { 8.0 };
    let avail_w = w - 2.0 * pad;
    let avail_h = h - 2.0 * pad - if header { HEADER_H + gap } else { 0.0 };
    let max_cols = (((avail_w + gap) / (MIN_CELL_W + gap)).floor() as usize).max(1);
    let min_cols = (((avail_w + gap) / (MAX_CELL_W + gap)).ceil() as usize).clamp(1, max_cols);
    let height_for = |rows: usize| (avail_h - gap * (rows as f32 - 1.0)) / rows as f32;
    let roomy = (min_cols..=max_cols).find_map(|cols| {
        let rows = n.div_ceil(cols);
        (height_for(rows) >= TALL_H).then_some((cols, rows))
    });
    let (cols, rows) = roomy.unwrap_or_else(|| {
        let fit = (((avail_h + gap) / (MIN_CELL_H + gap)).floor() as usize).max(1);
        (max_cols, n.div_ceil(max_cols).min(fit))
    });
    Plan {
        header,
        pad,
        gap,
        cols,
        rows,
        cell_w: (avail_w - gap * (cols as f32 - 1.0)) / cols as f32,
        cell_h: height_for(rows).min(MAX_CELL_H),
    }
}

/// Type sizes and widths for a cell. Sizes are fixed per band, never scaled with the width.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Band {
    label: u32,
    name: u32,
    price: u32,
    pill: u32,
    note: u32,
    tag: u32,
    price_w: f32,
    pill_w: f32,
    pill_h: f32,
    pad: f32,
    two_lines: bool,
    show_name: bool,
    wide: bool,
}

fn band(cw: f32, ch: f32) -> Band {
    let two = ch >= TALL_H;
    if cw >= 540.0 {
        Band { label: 30, name: 16, price: 38, pill: 26, note: 16, tag: 13, price_w: 230.0, pill_w: 150.0, pill_h: 50.0, pad: 14.0, two_lines: two, show_name: two, wide: true }
    } else if cw >= 380.0 {
        Band { label: 26, name: 15, price: 30, pill: 21, note: 14, tag: 12, price_w: 150.0, pill_w: 108.0, pill_h: 42.0, pad: 12.0, two_lines: two, show_name: false, wide: false }
    } else {
        Band { label: 20, name: 14, price: 22, pill: 16, note: 13, tag: 11, price_w: 92.0, pill_w: 74.0, pill_h: 34.0, pad: 9.0, two_lines: false, show_name: false, wide: false }
    }
}

/// Green up, red down, grey for no move.
fn tone(pct: f64) -> Color {
    if pct > 0.004 {
        UP
    } else if pct < -0.004 {
        DOWN
    } else {
        MUTE
    }
}

fn tag_style(tag: Tag) -> (&'static str, Color) {
    match tag {
        Tag::Pre => ("PRE", AMBER),
        Tag::After => ("AFTER", VIOLET),
        Tag::Closed => ("CLOSED", MUTE),
        Tag::Delayed => ("DELAYED", MUTE),
        Tag::Perp => ("PERP", SKY),
    }
}

fn tag_pill(tag: Tag, size: u32, fade: bool) -> Node {
    let (label, color) = tag_style(tag);
    let color = shade(color, fade);
    row(
        props!(background: color.with_alpha(0.16), border_radius: 6.0, padding: 3.0, justify_content: Justify::Center, cross_align: CrossAlign::Center),
        [text(label, style!(size: size, weight: FontWeight::SEMIBOLD, color: color))],
    )
}

fn tag_dot(tag: Tag, fade: bool) -> Node {
    let (_, color) = tag_style(tag);
    canvas(props!(width: 9.0, height: 9.0), [Draw::circle(4.5, 4.5, 4.5, shade(color, fade))])
}

/// A color dimmed while the data is not coming in.
fn shade(color: Color, fade: bool) -> Color {
    if fade { color.with_alpha(0.45) } else { color }
}

/// The text under the price.
fn note_text(note: Note, kind: Kind, wide: bool) -> Option<(String, Color)> {
    match note {
        Note::None => None,
        Note::NoPreMarketTrades => Some(("No pre-market trades".to_string(), MUTE)),
        Note::PrevClose(p) => Some((join(&["Prev ", &format::price(p, kind)]), MUTE)),
        Note::PreMarket(pct) => pct.map(|p| (join(&[if wide { "Pre-market " } else { "Pre " }, &format::percent(p)]), tone(p))),
        Note::Volume(v) => Some((join(&["Vol $", &format::compact(v)]), MUTE)),
        Note::Close { price, pct } => {
            let mut s = join(&["Close ", &format::price(price, kind)]);
            if let Some(p) = pct {
                s.push(' ');
                s.push_str(&format::percent(p));
            }
            Some((s, MUTE))
        }
    }
}

fn join(parts: &[&str]) -> String {
    parts.concat()
}

/// The company's name as the service writes it; for coins the part before the slash ("Bitcoin/USD ...").
fn display_name(item: &Row) -> Option<String> {
    let name = item.quote.as_ref()?.name.as_str();
    let name = if item.kind() == Kind::Crypto { name.split('/').next().unwrap_or(name) } else { name };
    (!name.is_empty()).then(|| name.to_string())
}

fn cell(item: &Row, b: Band, cw: f32, ch: f32, now_ms: u64, fade: bool) -> Node {
    let view: Option<View> = item.quote.as_ref().map(Quote::view);

    // Left: the symbol with its session tag, and the company name under it.
    let mut head = vec![text(
        item.entry.label.clone(),
        style!(size: b.label, weight: FontWeight::BOLD, color: shade(INK, fade), text_overflow: TextOverflow::Ellipsis),
    )];
    if let Some(tag) = view.and_then(|v| v.tag) {
        // Only a wide cell has room for the word; the others get a dot of its color.
        head.push(if b.wide { tag_pill(tag, b.tag, fade) } else { tag_dot(tag, fade) });
    }
    let mut left = vec![row(props!(gap: 8.0, cross_align: CrossAlign::Center), head)];
    if b.show_name
        && let Some(name) = display_name(item) {
            left.push(text(name, style!(size: b.name, color: shade(MUTE, fade), text_overflow: TextOverflow::Ellipsis)));
        }
    let left = col(props!(flex: 1.0, gap: 2.0, justify_content: Justify::Center), left);

    // Middle: the price, flashing for a moment when it ticks, and the line under it.
    let flash = item.tick.filter(|t| now_ms.saturating_sub(t.at_ms) < FLASH_MS);
    let price_color = match flash {
        Some(t) if t.up => UP,
        Some(_) => DOWN,
        None => INK,
    };
    let mut middle = Vec::new();
    match (&view, item.unknown) {
        (Some(v), _) => {
            middle.push(text(
                format::price(v.price, item.kind()),
                style!(size: b.price, weight: FontWeight::BOLD, color: shade(price_color, fade), align: TextAlign::Right, width: b.price_w),
            ));
            if b.two_lines
                && let Some((line, color)) = note_text(v.note, item.kind(), b.wide) {
                    middle.push(text(
                        line,
                        style!(size: b.note, color: shade(color, fade), align: TextAlign::Right, width: b.price_w, text_overflow: TextOverflow::Ellipsis),
                    ));
                }
        }
        (None, true) => middle.push(text(
            "No data",
            style!(size: b.note + 4, color: DIM, align: TextAlign::Right, width: b.price_w),
        )),
        (None, false) => middle.push(text(
            "...",
            style!(size: b.price, weight: FontWeight::BOLD, color: DIM, align: TextAlign::Right, width: b.price_w),
        )),
    }
    let middle = col(
        props!(width: b.price_w, gap: 1.0, cross_align: CrossAlign::End, justify_content: Justify::Center),
        middle,
    );

    // Right: the move.
    let (pill_text, pill_color) = match view.and_then(|v| v.pct) {
        Some(p) => (format::percent(p), shade(tone(p), fade)),
        None => (String::new(), DIM),
    };
    let pill = row(
        props!(width: b.pill_w, height: b.pill_h, background: pill_color.with_alpha(0.16), border_radius: 10.0, justify_content: Justify::Center, cross_align: CrossAlign::Center),
        [text(pill_text, style!(size: b.pill, weight: FontWeight::SEMIBOLD, color: pill_color))],
    );

    row(
        props!(width: cw, height: ch, background: CARD, border_radius: 14.0, padding: b.pad, gap: 12.0, cross_align: CrossAlign::Center),
        [left, middle, pill],
    )
}

fn dot(color: Color) -> Node {
    canvas(props!(width: 12.0, height: 12.0), [Draw::circle(6.0, 6.0, 6.0, color)])
}

fn clock(ts: i64) -> String {
    format_time(SystemTime { unix_secs: ts }, FormatTimeOpts::default())
}

fn header(width: f32, state: &State, now: i64) -> Node {
    let (market, market_color) = match state.market_session(now) {
        Some(Session::Regular) => ("Market open", UP),
        Some(Session::Pre) => ("Pre-market", AMBER),
        Some(Session::Post) => ("After hours", VIOLET),
        Some(Session::Closed) => ("Market closed", MUTE),
        None => ("", MUTE),
    };
    let (dot_color, status) = match state.health(now) {
        Health::Loading => (DIM, "Loading".to_string()),
        Health::Live => (UP, join(&["Updated ", &clock(state.last_ok)])),
        Health::Late => (AMBER, join(&["Late, updated ", &clock(state.last_ok)])),
        Health::Offline => (DOWN, join(&["Offline, last update ", &clock(state.last_ok)])),
    };
    let mut title = vec![text("Live Markets", style!(size: 22, weight: FontWeight::SEMIBOLD, color: INK))];
    let mut chips = Vec::new();
    if !market.is_empty() {
        chips.push((market, market_color));
    }
    // Perpetuals trade round the clock: say where they come from.
    if state.on_hl() {
        chips.push(("Hyperliquid 24/7", SKY));
    }
    for (label, color) in chips {
        title.push(row(
            props!(background: color.with_alpha(0.16), border_radius: 8.0, padding: 5.0, cross_align: CrossAlign::Center),
            [text(label, style!(size: 15, weight: FontWeight::SEMIBOLD, color: color))],
        ));
    }
    row(
        props!(width: width, height: HEADER_H, justify_content: Justify::SpaceBetween, cross_align: CrossAlign::Center),
        [
            row(props!(gap: 14.0, cross_align: CrossAlign::Center), title),
            row(
                props!(gap: 10.0, cross_align: CrossAlign::Center),
                [dot(dot_color), text(status, style!(size: 18, color: MUTE))],
            ),
        ],
    )
}

/// Page markers, when there are more symbols than one page holds.
#[expect(clippy::cast_precision_loss, reason = "a few pages")]
fn page_dots(page: usize, pages: usize, width: f32) -> Node {
    let step = 16.0;
    let total = step * pages as f32;
    let draws: Vec<Draw> = (0..pages)
        .map(|i| {
            let color = if i == page { INK } else { DIM };
            Draw::circle(step * i as f32 + step / 2.0, 4.0, 3.0, color)
        })
        .collect();
    row(
        props!(inset_left: 0.0, inset_bottom: 3.0, width: width, justify_content: Justify::Center),
        [canvas(props!(width: total, height: 8.0), draws)],
    )
}

fn message(width: f32, height: f32, line: &str) -> Node {
    col(
        props!(width: width, height: height, background: BG, justify_content: Justify::Center, cross_align: CrossAlign::Center),
        [text(line, style!(size: 24, color: MUTE))],
    )
}

/// Everything sits in the same place whatever it covers: one transparent area over the screen takes the taps.
fn tap_area(w: f32, h: f32) -> Node {
    // A rectangle nobody can see, so that there is something drawn where taps are taken.
    let invisible = Draw::rect(0.0, 0.0, w, h, Color::from_rgba(0, 0, 0, 0));
    touchable("tap", props!(inset_left: 0.0, inset_top: 0.0, width: w, height: h), [invisible])
}

fn list_screen(state: &State, w: u32, h: u32, now: i64) -> (Node, Hits) {
    let (wf, hf) = (w as f32, h as f32);
    let plan = plan(wf, hf, state.rows.len());
    let per_page = plan.per_page();
    let pages = state.rows.len().div_ceil(per_page).max(1);
    let page = ((state.uptime_ms / PAGE_MS) % pages as u64) as usize;
    let first = page * per_page;
    let items = &state.rows[first..state.rows.len().min(first + per_page)];
    let b = band(plan.cell_w, plan.cell_h);
    let fade = state.health(now) == Health::Offline;

    let mut content = Vec::new();
    if plan.header {
        content.push(header(wf - 2.0 * plan.pad, state, now));
    }
    let grid: Vec<Node> = items
        .chunks(plan.cols)
        .map(|line| {
            row(
                props!(gap: plan.gap),
                line.iter().map(|item| cell(item, b, plan.cell_w, plan.cell_h, state.uptime_ms, fade)).collect::<Vec<_>>(),
            )
        })
        .collect();
    content.push(col(props!(gap: plan.gap), grid));

    let mut children = vec![col(props!(width: wf, height: hf, padding: plan.pad, gap: plan.gap), content)];
    if pages > 1 {
        children.push(page_dots(page, pages, wf));
    }
    children.push(tap_area(wf, hf));
    let cells = (0..items.len()).map(|k| plan.cell(k)).collect();
    (col(props!(width: wf, height: hf, background: BG), children), Hits::List { cells, first })
}

// ── A symbol's chart ───────────────────────────────────────────────

/// A solid triangle pointing left or right in a box `w` by `h`, the sign of the arrows that move the cursor (the
/// chevrons beside the price step to another symbol).
fn triangle_draw(w: f32, h: f32, left: bool, color: Color) -> Draw {
    let points = if left {
        vec![(w * 0.64, h * 0.27), (w * 0.64, h * 0.73), (w * 0.34, h * 0.5)]
    } else {
        vec![(w * 0.36, h * 0.27), (w * 0.36, h * 0.73), (w * 0.66, h * 0.5)]
    };
    Draw::fill_path(points, color, false)
}

fn chevron(w: f32, h: f32, left: bool, color: Color) -> Node {
    let (near, far) = if left { (w * 0.7, w * 0.3) } else { (w * 0.3, w * 0.7) };
    canvas(
        props!(width: w, height: h),
        [Draw::path(vec![(near, h * 0.12), (far, h * 0.5), (near, h * 0.88)], 3.0, color, false, Interpolation::Linear)],
    )
}

fn button_box(r: Rect, background: Color, content: Vec<Node>) -> Node {
    row(
        props!(inset_left: r.x, inset_top: r.y, width: r.w, height: r.h, background: background, border_radius: 12.0, gap: 6.0, justify_content: Justify::Center, cross_align: CrossAlign::Center),
        content,
    )
}

fn frame_button(r: Rect, label: &str, selected: bool, size: u32) -> Node {
    let (background, color, weight) = if selected {
        (INK.with_alpha(0.16), INK, FontWeight::BOLD)
    } else {
        (CARD, MUTE, FontWeight::SEMIBOLD)
    };
    button_box(r, background, vec![text(label, style!(size: size, weight: weight, color: color))])
}

/// The drawing commands of a chart, in a canvas `cw` by `ch` whose plotting box leaves room for the labels.
#[expect(clippy::too_many_arguments, clippy::cast_precision_loss, reason = "the box, its margins, the type size and the cursor")]
fn chart_draws(plot: &Plot, kind: Kind, cw: f32, ch: f32, axis_w: f32, label_h: f32, size: u32, cursor: Option<(usize, &str)>) -> Vec<Draw> {
    let (pw, ph) = (cw - axis_w, ch - label_h);
    let color = if plot.up { UP } else { DOWN };
    let mut draws = Vec::new();

    // Pre-market and after-hours stretches.
    for band in &plot.bands {
        draws.push(Draw::rect(band.x0, 0.0, (band.x1 - band.x0).max(0.0), ph, INK.with_alpha(0.05)));
        // Only a stretch as wide as a whole day's pre-market or after-hours gets its name, and only on a chart tall enough.
        if band.x1 - band.x0 >= 120.0 && ph >= 160.0 && cursor.is_none() {
            let (label, tint) = if band.phase == Phase::Pre { ("PRE", AMBER) } else { ("AFTER", VIOLET) };
            draws.push(Draw::text(
                band.x0 + 8.0,
                16.0,
                label,
                style!(size: size.saturating_sub(2), weight: FontWeight::SEMIBOLD, color: tint.with_alpha(0.8), valign: VerticalAlign::Baseline),
            ));
        }
    }
    // Horizontal grid lines with their prices.
    for (y, price) in &plot.y_ticks {
        draws.push(Draw::path(vec![(0.0, *y), (pw, *y)], 1.0, GRID, false, Interpolation::Linear));
        draws.push(Draw::text(
            cw,
            *y,
            format::price(*price, kind),
            style!(size: size, color: MUTE, align: TextAlign::Right, valign: VerticalAlign::Center),
        ));
    }
    if let Some(y) = plot.reference {
        draws.push(Draw::dashed_path(vec![(0.0, y), (pw, y)], 1.5, MUTE.with_alpha(0.7), 6.0, 6.0));
    }
    // The line, with the area under it.
    if let (Some(first), Some(last)) = (plot.line.first().copied(), plot.line.last().copied()) {
        let mut area = plot.line.clone();
        area.push((last.0, ph));
        area.push((first.0, ph));
        draws.push(Draw::fill_path(area, Fill::linear(0.0, color.with_alpha(0.30), color.with_alpha(0.0)), false));
        draws.push(Draw::path(plot.line.clone(), 3.0, color, false, Interpolation::Linear));
        draws.push(Draw::circle(last.0, last.1, 9.0, BG));
        draws.push(Draw::circle(last.0, last.1, 6.0, color));
    }
    // A finger on the chart: a line down to its time, a dot on the price, and the time above.
    if let Some((i, label)) = cursor
        && let Some(&(x, y)) = plot.line.get(i)
    {
        draws.push(Draw::path(vec![(x, 0.0), (x, ph)], 1.5, INK.with_alpha(0.6), false, Interpolation::Linear));
        draws.push(Draw::circle(x, y, 12.0, BG));
        draws.push(Draw::circle(x, y, 9.0, color));
        draws.push(Draw::circle(x, y, 3.5, BG));
        // Half the label's width, near enough to keep it inside the box.
        let reach = label.len() as f32 * size as f32 * 0.3;
        let (lx, align) = if x - reach < 0.0 {
            (0.0, TextAlign::Left)
        } else if x + reach > pw {
            (pw, TextAlign::Right)
        } else {
            (x, TextAlign::Center)
        };
        draws.push(Draw::text(
            lx,
            16.0,
            label,
            style!(size: size + 1, weight: FontWeight::SEMIBOLD, color: INK, align: align, valign: VerticalAlign::Baseline, outline_color: BG, outline_width: 4.0),
        ));
    }
    // Labels along the bottom.
    for tick in &plot.x_ticks {
        let (x, align) = if tick.at < 28.0 {
            (0.0, TextAlign::Left)
        } else if tick.at > pw - 28.0 {
            (pw, TextAlign::Right)
        } else {
            (tick.at, TextAlign::Center)
        };
        draws.push(Draw::text(
            x,
            ph + label_h - 8.0,
            tick.label.clone(),
            style!(size: size, color: MUTE, align: align, valign: VerticalAlign::Baseline),
        ));
    }
    draws
}

fn chart_message(cw: f32, ch: f32, line: &str) -> Vec<Draw> {
    vec![Draw::text(cw / 2.0, ch / 2.0, line, style!(size: 20, color: MUTE, align: TextAlign::Center, valign: VerticalAlign::Center))]
}

/// The symbol's screen: its price, a chart of the chosen time frame, and the buttons to change it.
#[expect(clippy::cast_precision_loss, reason = "a screen size in pixels")]
fn detail_screen(state: &State, d: &Detail, w: u32, h: u32, now: i64) -> (Node, Hits) {
    let (wf, hf) = (w as f32, h as f32);
    let mut layout = detail_layout(wf, hf);
    let item = &state.rows[d.row];
    let kind = item.kind();
    let view = item.quote.as_ref().map(Quote::view);
    let class = layout.class;
    let compact = class == DetailClass::Compact;
    let fade = state.health(now) == Health::Offline;

    // The chart: the bars, with the live price as the last point.
    let phase = match view.and_then(|v| v.tag) {
        Some(Tag::Pre) => Phase::Pre,
        Some(Tag::After) => Phase::Post,
        _ => Phase::Regular,
    };
    let (axis_w, label_h) = chart_margins(class);
    let label_size = match class {
        DetailClass::Full => 16,
        DetailClass::Medium => 15,
        DetailClass::Compact => 12,
    };
    let rect = layout.chart;
    let points = d.series.as_ref().map(|s| s.with_live(view.map(|v| v.price), now * 1_000, phase));
    let plot = points.as_deref().and_then(|p| {
        let reference = item.quote.as_ref().and_then(|q| q.prev_close);
        chart::plot(p, d.frame, kind, reference, rect.w - axis_w, rect.h - label_h)
    });
    // A finger on the chart: the point it is over.
    let scrub = d.scrub.zip(plot.as_ref()).and_then(|(s, p)| {
        let pw = rect.w - axis_w;
        let x = match s {
            Scrub::Px(x) => x,
            Scrub::Fraction(f) => f * pw,
        }
        .clamp(0.0, pw);
        let i = chart::nearest(&p.line, x)?;
        Some((i, *points.as_ref()?.get(i)?))
    });
    let scrub_label = scrub.map(|(_, p)| chart::point_label(&p, d.frame, kind));
    let cursor = scrub.map(|(i, _)| i).zip(scrub_label.as_deref());
    let draws = match (&plot, d.failed) {
        (Some(p), _) => chart_draws(p, kind, rect.w, rect.h, axis_w, label_h, label_size, cursor),
        (None, true) => chart_message(rect.w, rect.h, "Chart unavailable"),
        (None, false) => chart_message(rect.w, rect.h, "Loading chart..."),
    };
    // The chart takes the finger itself: a touch area inside the screen's own wins, being the smaller.
    let chart_node = touchable("scrub", props!(inset_left: rect.x, inset_top: rect.y, width: rect.w, height: rect.h), draws);

    // The move shown beside the price: the day's for a day, the whole frame's otherwise.
    let frame_move = points.as_ref().and_then(|p| match (p.first(), p.last()) {
        (Some(a), Some(b)) if a.close != 0.0 => Some((b.close / a.close - 1.0) * 100.0),
        _ => None,
    });
    let pct = if d.frame == Frame::Day { view.and_then(|v| v.pct) } else { frame_move.or(view.and_then(|v| v.pct)) };
    // Under a finger the header reads the price then, and its move from where the frame began.
    let (shown_price, pct) = match scrub {
        Some((_, p)) => {
            let day_base = if d.frame == Frame::Day { item.quote.as_ref().and_then(|q| q.prev_close) } else { None };
            let base = day_base.or_else(|| points.as_ref().and_then(|v| v.first()).map(|f| f.close));
            (Some(p.close), base.filter(|b| *b != 0.0).map(|b| (p.close / b - 1.0) * 100.0))
        }
        None => (view.map(|v| v.price), pct),
    };
    let (label_px, name_px, price_px, pill_px) = match class {
        DetailClass::Full => (34, 17, 40, 26),
        DetailClass::Medium => (28, 15, 30, 21),
        DetailClass::Compact => (22, 13, 20, 15),
    };

    // Header: back, the symbol and its name, the price and its move, and the arrows to the next symbol.
    let mut children = Vec::new();
    let mut back = vec![chevron(if compact { 16.0 } else { 14.0 }, 24.0, true, INK)];
    if !compact {
        back.push(text("Back", style!(size: 20, weight: FontWeight::SEMIBOLD, color: INK)));
    }
    children.push(button_box(layout.back, CARD, back));
    for (r, left) in [(layout.prev, true), (layout.next, false)] {
        if let Some(r) = r {
            children.push(button_box(r, CARD, vec![chevron(14.0, 24.0, left, INK)]));
        }
    }

    let mut title_head = vec![text(item.entry.label.clone(), style!(size: label_px, weight: FontWeight::BOLD, color: INK))];
    // The session of the price shown: of the point under a finger, else of the live one.
    let shown_tag = match scrub {
        Some((_, p)) => match p.phase {
            Phase::Pre => Some(Tag::Pre),
            Phase::Post => Some(Tag::After),
            Phase::Regular => None,
        },
        None => view.and_then(|v| v.tag),
    };
    if let Some(tag) = shown_tag {
        // The word only where the header has room for it.
        title_head.push(if class == DetailClass::Full { tag_pill(tag, 13, false) } else { tag_dot(tag, false) });
    }
    let mut title = vec![row(props!(gap: 10.0, cross_align: CrossAlign::Center), title_head)];
    if !compact {
        if let Some(label) = &scrub_label {
            title.push(text(label.clone(), style!(size: name_px, weight: FontWeight::SEMIBOLD, color: INK, text_overflow: TextOverflow::Ellipsis, max_width: 360)));
        } else if let Some(name) = display_name(item) {
            title.push(text(name, style!(size: name_px, color: MUTE, text_overflow: TextOverflow::Ellipsis, max_width: 360)));
        }
    }
    let price_text = shown_price.map_or_else(|| "...".to_string(), |p| format::price(p, kind));
    let (pill_text, pill_color) = match pct {
        Some(p) => (format::percent(p), tone(p)),
        None => (String::new(), DIM),
    };
    let head_h = layout.back.h;
    let price_block = row(
        props!(gap: 12.0, cross_align: CrossAlign::Center),
        [
            text(price_text, style!(size: price_px, weight: FontWeight::BOLD, color: shade(INK, fade))),
            row(
                props!(background: pill_color.with_alpha(0.16), border_radius: 10.0, padding: if compact { 5.0 } else { 9.0 }, justify_content: Justify::Center, cross_align: CrossAlign::Center),
                [text(pill_text, style!(size: pill_px, weight: FontWeight::SEMIBOLD, color: pill_color))],
            ),
        ],
    );
    children.push(row(
        props!(inset_left: layout.title_x, inset_top: layout.pad, width: layout.price_edge - layout.title_x, height: head_h, gap: 12.0, justify_content: Justify::SpaceBetween, cross_align: CrossAlign::Center),
        [col(props!(gap: 1.0), title), price_block],
    ));
    children.push(chart_node);

    // Time frames, and the range of the chart beside them.
    let button_size = match class {
        DetailClass::Full => 20,
        DetailClass::Medium => 18,
        DetailClass::Compact => 14,
    };
    for (r, frame) in layout.frames.iter().zip(Frame::ALL) {
        children.push(frame_button(*r, frame.label(kind), frame == d.frame, button_size));
    }
    if class == DetailClass::Full {
        let frame_row = layout.frames[0];
        let text_edge = layout.jog_left.map_or(wf - layout.pad, |r| r.x - 18.0);
        if scrub.is_some() {
            // The cursor is on the chart: how to move it, and the button that lets it go.
            let live = Rect::new(wf - layout.pad - LIVE_W, frame_row.y, LIVE_W, frame_row.h);
            layout.live = Some(live);
            children.push(button_box(
                live,
                UP.with_alpha(0.16),
                vec![dot(UP), text("Live", style!(size: 20, weight: FontWeight::SEMIBOLD, color: UP))],
            ));
            children.push(row(
                props!(inset_right: wf - text_edge, inset_top: frame_row.y, height: frame_row.h, cross_align: CrossAlign::Center),
                [text("Touch the chart, or hold an arrow", style!(size: 17, color: MUTE))],
            ));
        } else if let Some(p) = &points {
            let lo = p.iter().map(|x| x.close).fold(f64::INFINITY, f64::min);
            let hi = p.iter().map(|x| x.close).fold(f64::NEG_INFINITY, f64::max);
            let line = join(&["Low ", &format::price(lo, kind), "   High ", &format::price(hi, kind)]);
            children.push(row(
                props!(inset_right: wf - text_edge, inset_top: frame_row.y, height: frame_row.h, cross_align: CrossAlign::Center),
                [text(line, style!(size: 17, color: MUTE))],
            ));
        }
    }
    // The arrows: a touch area each, smaller than the chart or the screen, so they take the finger first.
    for (r, key, left) in [(layout.jog_left, "jog_l", true), (layout.jog_right, "jog_r", false)] {
        let Some(r) = r else { continue };
        let held = state.jog == Some(if left { -1 } else { 1 });
        children.push(touchable(
            key,
            props!(inset_left: r.x, inset_top: r.y, width: r.w, height: r.h, background: if held { INK.with_alpha(0.22) } else { CARD }, border_radius: 12.0),
            [triangle_draw(r.w, r.h, left, INK)],
        ));
    }
    children.push(tap_area(wf, hf));
    (col(props!(width: wf, height: hf, background: BG), children), Hits::Detail(layout))
}

/// The whole screen for a `w` by `h` tile, and where its buttons are. `now` is Unix time in seconds.
#[must_use]
pub fn screen(state: &State, w: u32, h: u32, now: i64) -> (Node, Hits) {
    let (wf, hf) = (w as f32, h as f32);
    if state.rows.is_empty() {
        return (message(wf, hf, "Add symbols in the widget settings."), Hits::List { cells: Vec::new(), first: 0 });
    }
    match &state.screen {
        Screen::Detail(d) if d.row < state.rows.len() => detail_screen(state, d, w, h, now),
        _ => list_screen(state, w, h, now),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(w: f32, h: f32, n: usize) -> Plan {
        plan(w, h, n)
    }

    #[test]
    fn the_full_screen_with_eight_symbols_is_two_columns_of_four() {
        let plan = p(1280.0, 480.0, 8);
        assert!(plan.header);
        assert_eq!((plan.cols, plan.rows), (2, 4));
        assert!(plan.cell_w >= 540.0, "wide cells: {}", plan.cell_w);
        assert!(plan.cell_h >= TALL_H, "tall cells: {}", plan.cell_h);
        assert_eq!(plan.per_page(), 8);
    }

    #[test]
    fn the_full_screen_with_twelve_symbols_is_three_columns_of_four() {
        let plan = p(1280.0, 480.0, 12);
        assert_eq!((plan.cols, plan.rows), (3, 4));
    }

    #[test]
    fn a_few_symbols_do_not_stretch_into_one_wide_banner() {
        let plan = p(1280.0, 480.0, 3);
        assert_eq!(plan.cols, 2);
        assert!(plan.cell_h <= MAX_CELL_H);
    }

    #[test]
    fn the_tall_half_screen_has_two_narrow_columns() {
        let plan = p(638.0, 480.0, 8);
        assert_eq!((plan.cols, plan.rows), (2, 4));
    }

    #[test]
    fn small_tiles_take_turns_through_the_list() {
        let wide = p(638.0, 238.0, 8);
        assert!(!wide.header);
        assert!(wide.per_page() < 8);
        let small = p(317.0, 238.0, 8);
        assert_eq!(small.cols, 1);
        assert!(small.per_page() < 8 && small.per_page() >= 3);
        assert!(small.cell_h >= MIN_CELL_H - 1.0);
    }

    #[test]
    fn cells_pick_type_sizes_by_their_width_not_scale_them() {
        assert_eq!(band(608.0, 90.0).price, 38);
        assert_eq!(band(402.0, 90.0).price, 30);
        assert_eq!(band(296.0, 60.0).price, 22);
        assert!(band(608.0, 90.0).two_lines);
        assert!(!band(608.0, 60.0).two_lines);
    }

    fn cells_of(plan: &Plan, n: usize) -> Hits {
        Hits::List { cells: (0..n).map(|k| plan.cell(k)).collect(), first: 0 }
    }

    #[test]
    fn a_tap_on_a_cell_opens_that_symbol() {
        let plan = p(1280.0, 480.0, 8);
        let hits = cells_of(&plan, 8);
        // The middle of the 2nd cell of the 3rd row (index 5).
        let c = plan.cell(5);
        assert_eq!(resolve(&hits, c.x + c.w / 2.0, c.y + c.h / 2.0), Tap::Open(5));
        let first = plan.cell(0);
        assert_eq!(resolve(&hits, first.x + 5.0, first.y + 5.0), Tap::Open(0));
        // The header and the margin open nothing.
        assert_eq!(resolve(&hits, 600.0, 20.0), Tap::Nothing);
    }

    #[test]
    fn a_later_page_counts_from_its_first_symbol() {
        let plan = p(317.0, 238.0, 8);
        let hits = Hits::List { cells: (0..3).map(|k| plan.cell(k)).collect(), first: 3 };
        let c = plan.cell(1);
        assert_eq!(resolve(&hits, c.x + c.w / 2.0, c.y + c.h / 2.0), Tap::Open(4));
    }

    #[test]
    fn the_detail_screen_fits_every_tile_and_its_buttons_do_not_overlap() {
        for (w, h) in [(1280.0, 480.0), (638.0, 480.0), (638.0, 238.0), (317.0, 238.0)] {
            let l = detail_layout(w, h);
            assert!(l.chart.w > 100.0 && l.chart.h > 60.0, "{w}x{h}: chart {:?}", l.chart);
            assert!(l.chart.y + l.chart.h <= l.frames[0].y, "{w}x{h}: the chart ends above the buttons");
            assert!(l.frames[5].x + l.frames[5].w <= w, "{w}x{h}: the last button is on screen");
            for pair in l.frames.windows(2) {
                assert!(pair[0].x + pair[0].w <= pair[1].x, "{w}x{h}: frame buttons overlap");
            }
            assert!(l.title_x < l.price_edge, "{w}x{h}: the title has room");
            for r in [l.jog_left, l.jog_right].into_iter().flatten() {
                assert!(r.x >= 0.0 && r.x + r.w <= w && r.y >= 0.0 && r.y + r.h <= h, "{w}x{h}: an arrow is on screen");
            }
            if l.class == DetailClass::Full {
                let (a, b) = (l.jog_left.expect("BUG: arrows on the full screen"), l.jog_right.expect("BUG: arrows on the full screen"));
                assert!(l.frames[5].x + l.frames[5].w < a.x && a.x + a.w <= b.x, "the arrows sit right of the frame buttons");
                assert!(b.x + b.w <= w - l.pad - LIVE_W, "and left of Live");
            }
            assert!(l.plot_w() > 40.0);
        }
    }

    #[test]
    fn taps_on_the_detail_screen_press_the_button_under_them() {
        let l = detail_layout(1280.0, 480.0);
        let hits = Hits::Detail(l.clone());
        let mid = |r: Rect| (r.x + r.w / 2.0, r.y + r.h / 2.0);
        let (x, y) = mid(l.back);
        assert_eq!(resolve(&hits, x, y), Tap::Back);
        let (x, y) = mid(l.prev.expect("BUG: arrows on the full screen"));
        assert_eq!(resolve(&hits, x, y), Tap::Step(-1));
        let (x, y) = mid(l.next.expect("BUG: arrows on the full screen"));
        assert_eq!(resolve(&hits, x, y), Tap::Step(1));
        for (i, frame) in Frame::ALL.iter().enumerate() {
            let (x, y) = mid(l.frames[i]);
            assert_eq!(resolve(&hits, x, y), Tap::Frame(*frame));
        }
        let (x, y) = mid(l.chart);
        assert_eq!(resolve(&hits, x, y), Tap::Nothing);
    }

    #[test]
    fn small_tiles_have_no_arrows_but_keep_the_back_button() {
        let l = detail_layout(317.0, 238.0);
        assert!(l.prev.is_none() && l.next.is_none());
        let hits = Hits::Detail(l.clone());
        assert_eq!(resolve(&hits, l.back.x + 4.0, l.back.y + 4.0), Tap::Back);
    }

    #[test]
    fn every_screen_builds_at_every_size_with_a_finger_on_the_chart() {
        use crate::chart::{RawBar, Series};
        use crate::symbols::{Source, resolve_all};

        let mut state = State { interval_ms: 2_000, ..State::default() };
        state.set_entries(resolve_all(&["NVDA", "TSLA"]), Source::Cnbc);
        let bars: Vec<RawBar> = (0..120)
            .map(|i| RawBar { ms: i * 60_000, local: 20_780 * 86_400 + 4 * 3_600 + i * 60, close: 100.0 + (i as f64 / 7.0).sin() })
            .collect();
        for frame in Frame::ALL {
            for (w, h) in [(1280, 480), (638, 480), (638, 238), (317, 238)] {
                for finger in [None, Some(Scrub::Px(-40.0)), Some(Scrub::Px(123.0)), Some(Scrub::Px(5_000.0)), Some(Scrub::Fraction(0.5))] {
                    let mut detail = Detail::new(0, frame);
                    detail.series = Some(Series::from_bars(frame, Kind::Stock, &bars));
                    detail.scrub = finger;
                    state.screen = Screen::Detail(detail);
                    let (_, hits) = screen(&state, w, h, 1_000);
                    assert!(matches!(hits, Hits::Detail(_)), "{frame:?} {w}x{h}");
                }
                state.screen = Screen::List;
                let (_, hits) = screen(&state, w, h, 1_000);
                assert!(matches!(hits, Hits::List { .. }), "{w}x{h}");
            }
        }
    }

    #[test]
    fn the_live_button_is_pressed_before_anything_under_it() {
        let mut l = detail_layout(1280.0, 480.0);
        let live = Rect::new(1000.0, 400.0, 150.0, 48.0);
        l.live = Some(live);
        let hits = Hits::Detail(l);
        assert_eq!(resolve(&hits, live.x + 10.0, live.y + 10.0), Tap::Live);
        assert_eq!(resolve(&hits, 5.0, 5.0), Tap::Nothing);
    }

    #[test]
    fn a_perpetual_builds_at_every_size_with_its_tag_volume_and_a_finger_on_the_chart() {
        use crate::chart::{RawBar, Series};
        use crate::hl::Ctx;
        use crate::symbols::{Source, resolve_all};

        let mut state = State { interval_ms: 2_000, ..State::default() };
        state.set_entries(resolve_all(&["NVDA", "BTC-USD", "JPM"]), Source::Hyperliquid);
        let ctx = Ctx { mark: "163.43".into(), mid: String::new(), prev_day: "134.51".into(), volume: "1615050662.07".into() };
        assert!(state.apply_hl("xyz:NVDA", &ctx, 1_000));
        assert_eq!(state.rows[0].kind(), Kind::Perp);
        let bars: Vec<RawBar> = (0..120)
            .map(|i| RawBar { ms: i * 300_000, local: 20_780 * 86_400 + i * 300, close: 160.0 + (i as f64 / 7.0).sin() })
            .collect();
        for frame in Frame::ALL {
            for (w, h) in [(1280, 480), (638, 480), (638, 238), (317, 238)] {
                for finger in [None, Some(Scrub::Px(123.0)), Some(Scrub::Fraction(0.5))] {
                    let mut detail = Detail::new(0, frame);
                    detail.series = Some(Series::from_bars(frame, Kind::Perp, &bars));
                    detail.scrub = finger;
                    state.screen = Screen::Detail(detail);
                    let (_, hits) = screen(&state, w, h, 1_000);
                    assert!(matches!(hits, Hits::Detail(_)), "{frame:?} {w}x{h}");
                }
                state.screen = Screen::List;
                let (_, hits) = screen(&state, w, h, 1_000);
                assert!(matches!(hits, Hits::List { .. }), "{w}x{h}");
            }
        }
    }

    #[test]
    fn the_perpetual_tag_and_the_volume_note_read_well() {
        assert_eq!(tag_style(Tag::Perp).0, "PERP");
        let (line, _) = note_text(Note::Volume(1_615_050_662.0), Kind::Perp, true).expect("BUG: a volume has a line");
        assert_eq!(line, "Vol $1.6B");
    }
}
