//! Platform-agnostic macOS Accessibility tree decision logic.
//!
//! Matches `ReadKakaoAX.swift`, `ControlKakaoAX.swift`, and `SendKakao.swift`
//! and can be tested on any OS using synthetic AX trees.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::error::{KaxError, Result};

pub const COMPOSER_LABELS: &[&str] = &[
    "메시지 입력",
    "Message",
    "Type a message",
    "Enter a message",
];

pub const SEND_BUTTON_TITLES: &[&str] = &["전송", "Send"];

/// Number of icon buttons that follow the in-room search field (previous, next, and three
/// more toolbar actions). The buttons carry no `AXDescription`/`AXTitle`/`AXHelp`, and their
/// `AXIdentifier`s are Interface Builder auto-ids (`_NS:<n>`) that change between app builds
/// (seen: `_NS:10,30,43,56,80`, `_NS:10,31,45,73,85`, `_NS:10,31,45,59,85`), so the toolbar is
/// recognised structurally instead: see [`check_search_toolbar_geometry`].
pub const SEARCH_TOOLBAR_BUTTON_COUNT: usize = 5;

/// Screen frame of an AX element (`AXPosition` + `AXSize`), in points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AxFrame {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl AxFrame {
    fn center_y(&self) -> f64 {
        self.y + self.h / 2.0
    }
    fn right(&self) -> f64 {
        self.x + self.w
    }
}

/// Environment variable for the AX read *stall* timeout (`accessibility_read_limit`), in seconds:
/// a transcript read is cut only when it makes no progress (no row finished) for this long, so a
/// long transcript that is steadily being read is not cut, while a hung app still is. Decimals
/// are allowed; values are clamped to `MIN_READ_TIMEOUT_SECS..=MAX_READ_TIMEOUT_SECS`, and
/// unset/invalid values use the default. The whole tool call is bounded by [`CALL_TIMEOUT_ENV`].
pub const READ_TIMEOUT_ENV: &str = "KAX_READ_TIMEOUT_SECS";
pub const DEFAULT_READ_TIMEOUT_SECS: f64 = 8.0;
pub const MIN_READ_TIMEOUT_SECS: f64 = 1.0;
pub const MAX_READ_TIMEOUT_SECS: f64 = 120.0;

/// Parse a `KAX_READ_TIMEOUT_SECS` value into the read time budget.
pub fn parse_read_timeout(raw: Option<&str>) -> Duration {
    let secs = raw
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0)
        .map(|v| v.clamp(MIN_READ_TIMEOUT_SECS, MAX_READ_TIMEOUT_SECS))
        .unwrap_or(DEFAULT_READ_TIMEOUT_SECS);
    Duration::from_secs_f64(secs)
}

/// The AX read stall timeout, from `KAX_READ_TIMEOUT_SECS` or the 8 s default.
pub fn read_timeout() -> Duration {
    parse_read_timeout(std::env::var(READ_TIMEOUT_ENV).ok().as_deref())
}

/// Environment variable for the total AX time one tool call may spend (`tool_call_time_limit`),
/// in seconds, across all of its reads and history steps. Clamped to
/// `MIN_CALL_TIMEOUT_SECS..=MAX_CALL_TIMEOUT_SECS`; unset/invalid values use the default.
pub const CALL_TIMEOUT_ENV: &str = "KAX_CALL_TIMEOUT_SECS";
pub const DEFAULT_CALL_TIMEOUT_SECS: f64 = 120.0;
pub const MIN_CALL_TIMEOUT_SECS: f64 = 5.0;
pub const MAX_CALL_TIMEOUT_SECS: f64 = 600.0;

/// Parse a `KAX_CALL_TIMEOUT_SECS` value into the per-call time budget.
pub fn parse_call_timeout(raw: Option<&str>) -> Duration {
    let secs = raw
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0)
        .map(|v| v.clamp(MIN_CALL_TIMEOUT_SECS, MAX_CALL_TIMEOUT_SECS))
        .unwrap_or(DEFAULT_CALL_TIMEOUT_SECS);
    Duration::from_secs_f64(secs)
}

/// The per-call time budget, from `KAX_CALL_TIMEOUT_SECS` or the 120 s default.
pub fn call_timeout() -> Duration {
    parse_call_timeout(std::env::var(CALL_TIMEOUT_ENV).ok().as_deref())
}

thread_local! {
    /// Deadline of the tool call running on this thread. Thread-local because tool calls run
    /// synchronously on their caller's thread and `read_many` reads rooms on parallel worker
    /// threads, each of which must keep its own budget.
    static CALL_DEADLINE: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

/// Starts the time budget of one tool call on the current thread; it ends when the guard is
/// dropped. Nested calls on the same thread (e.g. `search_messages` opening the room, loading
/// history and reading) keep the outermost deadline, so repeated reads share one budget
/// instead of each getting a fresh one. Other threads are unaffected.
#[must_use]
pub struct CallBudget {
    outermost: bool,
    /// The guard belongs to the thread whose deadline it set.
    _not_send: std::marker::PhantomData<*const ()>,
}

impl CallBudget {
    pub fn begin() -> Self {
        Self::begin_with(call_timeout())
    }

    pub fn begin_with(budget: Duration) -> Self {
        let outermost = CALL_DEADLINE.with(|slot| {
            if slot.get().is_some() {
                return false;
            }
            slot.set(Some(Instant::now() + budget));
            true
        });
        Self { outermost, _not_send: std::marker::PhantomData }
    }
}

impl Drop for CallBudget {
    fn drop(&mut self) {
        if self.outermost {
            CALL_DEADLINE.with(|slot| slot.set(None));
        }
    }
}

/// Deadline of the tool call in progress on this thread, if one was started with
/// [`CallBudget::begin`].
pub fn call_deadline() -> Option<Instant> {
    CALL_DEADLINE.with(|slot| slot.get())
}

/// Whether the current tool call has used up its time budget.
pub fn call_budget_exhausted() -> bool {
    call_deadline().is_some_and(|d| Instant::now() >= d)
}

/// Pixel slack for the toolbar geometry checks (KakaoTalk overlaps adjacent buttons by 2pt).
const TOOLBAR_TOLERANCE: f64 = 4.0;

/// Structural guard for the search toolbar, independent of `AXIdentifier` values.
///
/// `field` is the search field's frame and `buttons` the frames of the
/// [`SEARCH_TOOLBAR_BUTTON_COUNT`] `AXButton` siblings that directly follow it. The toolbar is
/// accepted only when every button:
/// - sits on the same row as the field (vertical centres within half the taller height),
/// - lies to the right of the field,
/// - has the same size as the first button, and
/// - is ordered strictly left to right without overlapping its neighbour.
///
/// With this ordering `buttons[0]` is "previous result" and `buttons[1]` is "next result".
/// Any missing frame or failed check is `search_toolbar_layout_unknown` (fail closed).
pub fn check_search_toolbar_geometry(
    field: Option<AxFrame>,
    buttons: &[Option<AxFrame>],
) -> Result<()> {
    let unknown = || KaxError::blocked("search_toolbar_layout_unknown");
    if buttons.len() != SEARCH_TOOLBAR_BUTTON_COUNT {
        return Err(unknown());
    }
    let field = field.ok_or_else(unknown)?;
    let frames: Vec<AxFrame> = buttons.iter().map(|b| b.ok_or_else(unknown)).collect::<Result<_>>()?;
    let first = frames[0];
    if first.w <= 0.0 || first.h <= 0.0 {
        return Err(unknown());
    }
    for (i, b) in frames.iter().enumerate() {
        let same_row =
            (b.center_y() - field.center_y()).abs() <= b.h.max(field.h) / 2.0;
        let right_of_field = b.x >= field.right() - TOOLBAR_TOLERANCE;
        let same_size = (b.w - first.w).abs() <= TOOLBAR_TOLERANCE
            && (b.h - first.h).abs() <= TOOLBAR_TOLERANCE;
        if !same_row || !right_of_field || !same_size {
            return Err(unknown());
        }
        if i > 0 {
            let prev = frames[i - 1];
            if b.x <= prev.x || b.x < prev.right() - TOOLBAR_TOLERANCE {
                return Err(unknown());
            }
        }
    }
    Ok(())
}

/// Newest transcript rows one `read_messages` snapshot covers (its `offset` paging range).
pub const READ_ROW_CAP: usize = 1000;
/// Newest transcript rows a `search_messages` pass reads (driver action `read_all`). Rows older
/// than this are reported as an unsearched range instead of being silently skipped.
pub const SEARCH_ROW_CAP: usize = 5000;

pub const SELF_PROFILE_LABELS: &[&str] = &["내 기본프로필", "My Default Profile", "My Profile"];
pub const SELF_MENU_TITLES: &[&str] = &["나와의 채팅", "Chat with myself"];

/// Room-type labels that KakaoTalk Mac prepends to a room-list cell's name `AXValue` as
/// `"<label>, <room name>"` (e.g. `"오픈채팅, 수학 스터디-기출 모음방"`, `"팀채팅, 운영 조교방"`),
/// mapped to a stable `roomType`. The chat window's `AXTitle` carries only the room name.
pub const ROOM_TYPE_PREFIXES: &[(&str, &str)] = &[
    ("오픈채팅", "open_chat"),
    ("팀채팅", "team_chat"),
    ("Open Chat", "open_chat"),
    ("OpenChat", "open_chat"),
    ("Team Chat", "team_chat"),
];

/// Split a known room-type prefix (`"<label>, "`) off a room-list name.
/// Returns `(roomType, name without prefix)`; names without a known prefix are unchanged.
/// Only the listed labels are stripped, so room names that merely contain `", "` stay intact.
pub fn split_room_type_prefix(name: &str) -> (Option<&'static str>, &str) {
    for (label, kind) in ROOM_TYPE_PREFIXES {
        if let Some(rest) = name.strip_prefix(label).and_then(|r| r.strip_prefix(", ")) {
            if !rest.is_empty() {
                return (Some(kind), rest);
            }
        }
    }
    (None, name)
}

/// The room name as it appears in the chat window title (room-type prefix removed).
pub fn room_base_name(name: &str) -> &str {
    split_room_type_prefix(name).1
}

/// Which room-list entries a requested room name refers to.
#[derive(Debug, PartialEq)]
pub struct RoomNameMatch<'a> {
    /// The name without a type label, as the chat window is titled.
    pub base: &'a str,
    /// The request named a room type (`"팀채팅, A"`).
    pub typed: bool,
    /// Indices of entries with the same base name and, if typed, the same type.
    pub matching: Vec<usize>,
    /// Entries with the same base name but another type (only counted for typed requests).
    pub other_type: usize,
}

/// Matches a requested room name against room-list entry names. Names are compared without
/// the type label; a type label in the request must match the entry's type, so
/// `"팀채팅, A"` never picks `"오픈채팅, A"` or a plain `"A"`.
pub fn match_room_name<'a>(requested: &'a str, names: &[&str]) -> RoomNameMatch<'a> {
    let (requested_type, base) = split_room_type_prefix(requested);
    let mut matching = Vec::new();
    let mut other_type = 0;
    for (i, name) in names.iter().enumerate() {
        let (kind, entry_base) = split_room_type_prefix(name);
        if entry_base != base {
            continue;
        }
        if requested_type.is_none() || kind == requested_type {
            matching.push(i);
        } else {
            other_type += 1;
        }
    }
    RoomNameMatch { base, typed: requested_type.is_some(), matching, other_type }
}

/// Default and maximum number of rooms `rooms(scroll=true)` collects.
pub const DEFAULT_ROOM_SCROLL_LIMIT: i64 = 300;
pub const MAX_ROOM_SCROLL_LIMIT: i64 = 1000;

/// Scroll-bar `AXValue` step (0.0–1.0) that moves the room list by about one page.
///
/// Two rows of the previous page stay on screen so no row is skipped when the row heights or
/// the step rounding are slightly off; rows seen twice are dropped by their `AXIndex`.
pub fn room_scroll_step(visible_rows: usize, total_rows: usize) -> f64 {
    let hidden = total_rows.saturating_sub(visible_rows);
    if hidden == 0 {
        return 1.0;
    }
    let page = visible_rows.saturating_sub(2).max(1);
    (page as f64 / hidden as f64).min(1.0)
}

/// Consecutive scroll steps without a new row after which the room-list scroll gives up.
pub const ROOM_SCROLL_IDLE_STEPS: usize = 3;

/// Why the room-list scroll should stop after a page, or `None` to scroll on.
///
/// `collected` counts distinct rows read so far, `total` is the table's `AXRows` count,
/// `at_end` means the scroll bar already reached 1.0, and `idle_steps` counts consecutive pages
/// that added no row.
pub fn room_scroll_stop(
    collected: usize,
    total: usize,
    limit: usize,
    at_end: bool,
    idle_steps: usize,
) -> Option<&'static str> {
    if collected >= limit && limit < total {
        Some("limit")
    } else if collected >= total {
        Some("complete")
    } else if at_end {
        Some("end_of_list")
    } else if idle_steps >= ROOM_SCROLL_IDLE_STEPS {
        Some("no_new_rows")
    } else {
        None
    }
}

pub fn is_macos_self_alias(title: &str) -> bool {
    let trimmed = title.trim();
    matches!(
        trimmed.to_lowercase().as_str(),
        "나" | "나와의 채팅" | "chat with myself" | "me"
    )
}

/// Abstract representation of an AX attribute value.
#[derive(Debug, Clone, PartialEq)]
pub enum AxValue {
    String(String),
    Bool(bool),
    Int(i64),
    Float(f64),
    Elements(Vec<usize>),
}

/// Synthetic or live macOS AX tree provider.
/// Elements are addressed by opaque `usize` handles owned by the tree.
pub trait AxTree {
    fn app_windows(&self) -> Result<Vec<usize>>;
    fn attr(&self, id: usize, name: &str) -> Result<Option<AxValue>>;
    fn string_attr(&self, id: usize, name: &str, _optional: bool) -> Result<String> {
        match self.attr(id, name)? {
            Some(AxValue::String(s)) => Ok(s),
            _ => Ok(String::new()),
        }
    }
    fn elements_attr(&self, id: usize, name: &str) -> Result<Vec<usize>> {
        match self.attr(id, name)? {
            Some(AxValue::Elements(v)) => Ok(v),
            _ => Ok(Vec::new()),
        }
    }
    fn bool_attr(&self, id: usize, name: &str) -> Result<Option<bool>> {
        match self.attr(id, name)? {
            Some(AxValue::Bool(b)) => Ok(Some(b)),
            _ => Ok(None),
        }
    }
    fn int_attr(&self, id: usize, name: &str) -> Result<Option<i64>> {
        match self.attr(id, name)? {
            Some(AxValue::Int(i)) => Ok(Some(i)),
            _ => Ok(None),
        }
    }
    fn attr_count(&self, id: usize, name: &str) -> Result<Option<usize>> {
        Ok(Some(self.elements_attr(id, name)?.len()))
    }
    /// `AXPosition` + `AXSize` of the element, if both are available.
    fn frame(&self, _id: usize) -> Result<Option<AxFrame>> {
        Ok(None)
    }
    fn is_settable(&self, _id: usize, _name: &str) -> bool {
        false
    }
    fn action_names(&self, _id: usize) -> Vec<String> {
        Vec::new()
    }
    fn perform_action(&self, _id: usize, _action: &str) -> Result<i32> {
        Ok(0)
    }
    fn set_string_attr(&self, _id: usize, _name: &str, _value: &str) -> Result<i32> {
        Ok(0)
    }
    fn same_element(&self, a: usize, b: usize) -> bool {
        a == b
    }
}

/// Select the unique enabled `AXMenuItem` with `AXIdentifier == "openChatRoom:"`
/// from the cell's `AXMenu` list. Matches `ControlKakaoAX.swift` lines 270-279.
pub fn select_open_menu_item<T: AxTree>(tree: &T, menus: &[usize]) -> Result<usize> {
    let mut open_items = Vec::new();
    for &menu in menus {
        for item in tree.elements_attr(menu, "AXChildren")? {
            if tree.string_attr(item, "AXRole", true)? == "AXMenuItem"
                && tree.string_attr(item, "AXIdentifier", true)? == "openChatRoom:"
                && tree.bool_attr(item, "AXEnabled")? == Some(true)
            {
                open_items.push(item);
            }
        }
    }
    if open_items.len() != 1 {
        return Err(KaxError::blocked("open_menu_item_not_unique"));
    }
    Ok(open_items[0])
}

#[derive(Debug, Clone)]
pub struct RoomEntry {
    pub cell: usize,
    pub name: String,
    pub is_self: bool,
    pub row_index: i64,
    pub labels: Vec<String>,
    pub preview: Option<String>,
}

impl RoomEntry {
    pub fn to_json(&self) -> Value {
        let (room_type, window_title) = split_room_type_prefix(&self.name);
        json!({
            "name": self.name,
            "windowTitle": window_title,
            "roomType": room_type,
            "isSelf": self.is_self,
            "rowIndex": self.row_index,
            "labels": self.labels,
            "preview": self.preview,
        })
    }
}

/// Collect nodes within a subtree, enforcing depth and modal dialog guards.
pub fn walk_control_nodes<T: AxTree>(
    tree: &T,
    el: usize,
    include_tables: bool,
    depth: usize,
    visits: &mut usize,
    deadline: Instant,
) -> Result<Vec<usize>> {
    *visits += 1;
    if *visits >= 20_000 || Instant::now() >= deadline {
        return Err(KaxError::blocked("accessibility_control_limit"));
    }
    if depth >= 20 {
        return Err(KaxError::blocked("accessibility_tree_depth"));
    }
    let role = tree.string_attr(el, "AXRole", true)?;
    if matches!(role.as_str(), "AXSheet" | "AXDialog") {
        return Err(KaxError::blocked("modal_dialog_present"));
    }
    let mut result = vec![el];
    if !include_tables && matches!(role.as_str(), "AXTable" | "AXOutline" | "AXRow" | "AXCell") {
        return Ok(result);
    }
    if matches!(role.as_str(), "AXTextArea" | "AXTextField" | "AXSearchField") {
        return Ok(result);
    }
    for child in tree.elements_attr(el, "AXChildren")? {
        result.extend(walk_control_nodes(
            tree,
            child,
            include_tables,
            depth + 1,
            visits,
            deadline,
        )?);
    }
    Ok(result)
}

/// Extract room entries from the main window's room list table.
/// When `names_only == true` (ordinary `open_room` without `require_self`),
/// never reads preview, time labels, row index, or self-badge metadata.
pub fn room_entries<T: AxTree>(
    tree: &T,
    main_window: usize,
    names_only: bool,
) -> Result<(Vec<RoomEntry>, usize, &'static str)> {
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut visits = 0usize;
    let all_nodes = walk_control_nodes(tree, main_window, false, 0, &mut visits, deadline)?;
    let mut tables = Vec::new();
    for id in all_nodes {
        if tree.string_attr(id, "AXRole", true)? == "AXTable" {
            tables.push(id);
        }
    }
    if tables.len() != 1 {
        return Err(KaxError::blocked("room_list_not_unique"));
    }
    let table = tables[0];

    let visible_attr = tree.attr(table, "AXVisibleRows")?;
    let (rows, source) = match visible_attr {
        Some(AxValue::Elements(v)) => (v, "AXVisibleRows"),
        _ => {
            let mut ch = Vec::new();
            for child in tree.elements_attr(table, "AXChildren")? {
                if tree.string_attr(child, "AXRole", true)? == "AXRow" {
                    ch.push(child);
                }
            }
            (ch, "AXChildren")
        }
    };
    let reported = tree.attr_count(table, "AXRows")?.unwrap_or(rows.len());
    if rows.len() > 1000 {
        return Err(KaxError::blocked("room_list_limit"));
    }

    let mut entries = Vec::new();
    for (index, row) in rows.into_iter().enumerate() {
        let mut cells = Vec::new();
        for child in tree.elements_attr(row, "AXChildren")? {
            if tree.string_attr(child, "AXRole", true)? == "AXCell" {
                cells.push(child);
            }
        }
        if cells.len() != 1 {
            continue;
        }
        let cell = cells[0];
        let children = tree.elements_attr(cell, "AXChildren")?;
        let mut label_elements = Vec::new();
        for child in children {
            if tree.string_attr(child, "AXRole", true)? == "AXStaticText" {
                label_elements.push(child);
            }
        }
        let Some(&name_el) = label_elements.first() else {
            continue;
        };
        let name = tree.string_attr(name_el, "AXValue", true)?;
        if name.is_empty() {
            continue;
        }
        if names_only {
            entries.push(RoomEntry {
                cell,
                name,
                is_self: false,
                row_index: index as i64,
                labels: Vec::new(),
                preview: None,
            });
            continue;
        }
        let mut labels = Vec::new();
        for el in &label_elements {
            let val = tree.string_attr(*el, "AXValue", true)?;
            if !val.is_empty() {
                labels.push(val);
            }
        }
        let descendants = walk_control_nodes(tree, cell, true, 0, &mut visits, deadline)?;
        let mut preview = None;
        let mut own = false;
        for d in descendants {
            let role = tree.string_attr(d, "AXRole", true)?;
            if role == "AXTextArea" && preview.is_none() {
                preview = Some(tree.string_attr(d, "AXValue", true)?);
            } else if role == "AXImage"
                && tree.string_attr(d, "AXDescription", true)? == "badge me"
            {
                own = true;
            }
        }
        let row_index = tree.int_attr(row, "AXIndex")?.unwrap_or(index as i64);
        entries.push(RoomEntry {
            cell,
            name,
            is_self: own,
            row_index,
            labels,
            preview,
        });
    }

    Ok((entries, reported, source))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoomLookupDecision {
    AlreadyOpen,
    OpenCell(usize),
}

/// Exact-room lookup guards matching `ControlKakaoAX.swift` lines 251-264.
///
/// Names are compared after removing a room-type prefix on both sides
/// ([`room_base_name`]), so `"시대-…"` and `"오픈채팅, 시대-…"` address the same room, and two
/// rooms that differ only by prefix are still `target_room_name_ambiguous`.
pub fn decide_room_open<T: AxTree>(
    tree: &T,
    entries: &[RoomEntry],
    title: &str,
    require_self: bool,
) -> Result<RoomLookupDecision> {
    if title.is_empty() {
        return Err(KaxError::blocked("exact_window_title_required"));
    }
    let title = room_base_name(title);
    let matching: Vec<&RoomEntry> = entries
        .iter()
        .filter(|e| room_base_name(&e.name) == title)
        .collect();
    if matching.len() > 1 {
        return Err(KaxError::blocked("target_room_name_ambiguous"));
    }
    if require_self {
        if matching.len() != 1 {
            return Err(KaxError::blocked("room_not_in_exposed_list"));
        }
        if !matching[0].is_self {
            return Err(KaxError::blocked("self_room_not_verified"));
        }
    }
    let mut opened = Vec::new();
    for w in tree.app_windows()? {
        if tree.string_attr(w, "AXTitle", true)? == title {
            opened.push(w);
        }
    }
    if !opened.is_empty() {
        if opened.len() != 1 {
            return Err(KaxError::blocked("target_room_window_ambiguous"));
        }
        return Ok(RoomLookupDecision::AlreadyOpen);
    }
    if matching.len() != 1 {
        return Err(KaxError::blocked("room_not_in_exposed_list"));
    }
    Ok(RoomLookupDecision::OpenCell(matching[0].cell))
}

/// Select the search toolbar buttons that directly follow `field` among its `siblings`
/// (`[previous, next, ...]`), validated structurally by [`check_search_toolbar_geometry`].
pub fn validate_search_toolbar_buttons<T: AxTree>(
    tree: &T,
    field: usize,
    siblings: &[usize],
) -> Result<Vec<usize>> {
    let Some(pos) = siblings.iter().position(|&s| tree.same_element(s, field)) else {
        return Err(KaxError::blocked("search_field_missing"));
    };
    let buttons: Vec<usize> = siblings
        .iter()
        .skip(pos + 1)
        .take(SEARCH_TOOLBAR_BUTTON_COUNT)
        .copied()
        .collect();
    if buttons.len() != SEARCH_TOOLBAR_BUTTON_COUNT {
        return Err(KaxError::blocked("search_toolbar_layout_unknown"));
    }
    let mut frames = Vec::with_capacity(buttons.len());
    for &btn in &buttons {
        if tree.string_attr(btn, "AXRole", true)? != "AXButton" {
            return Err(KaxError::blocked("search_toolbar_layout_unknown"));
        }
        frames.push(tree.frame(btn)?);
    }
    check_search_toolbar_geometry(tree.frame(field)?, &frames)?;
    Ok(buttons)
}

/// Select the unique official self-profile `AXCell` using the bidirectional `AXOutline`
/// disclosure relationship observed in KakaoTalk Mac `26.10.0`:
/// - `header_row`: `AXDisclosureLevel == 0`, contains `"내 기본프로필"` (`SELF_PROFILE_LABELS`),
///   and `AXDisclosedRows == [profile_row]`.
/// - `profile_row`: `AXDisclosedByRow == header_row`, `AXDisclosureLevel == 1`, and its unique
///   `AXCell` supports `AXShowMenu`.
/// Fails closed if any link, level, uniqueness, or action check fails (no positional guessing).
pub fn select_self_profile_cell<T: AxTree>(tree: &T, rows: &[usize]) -> Result<usize> {
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut visits = 0usize;

    let mut headers = Vec::new();
    for &row in rows {
        if tree.int_attr(row, "AXDisclosureLevel")? != Some(0) {
            continue;
        }
        let mut cells = Vec::new();
        for child in tree.elements_attr(row, "AXChildren")? {
            if tree.string_attr(child, "AXRole", true)? == "AXCell" {
                cells.push(child);
            }
        }
        if cells.len() != 1 {
            continue;
        }
        let descendants = walk_control_nodes(tree, cells[0], true, 0, &mut visits, deadline)?;
        let is_header = descendants.iter().any(|&d| {
            let role = tree.string_attr(d, "AXRole", true).unwrap_or_default();
            let desc = tree.string_attr(d, "AXDescription", true).unwrap_or_default();
            if SELF_PROFILE_LABELS.contains(&desc.as_str()) {
                return true;
            }
            if role == "AXStaticText" {
                let val = tree.string_attr(d, "AXValue", true).unwrap_or_default();
                return SELF_PROFILE_LABELS.contains(&val.as_str());
            }
            false
        });
        if is_header {
            headers.push(row);
        }
    }
    if headers.len() != 1 {
        return Err(KaxError::blocked("self_room_not_verified"));
    }
    let header_row = headers[0];

    let disclosed = tree.elements_attr(header_row, "AXDisclosedRows")?;
    if disclosed.len() != 1 {
        return Err(KaxError::blocked("self_room_not_verified"));
    }
    let profile_row = disclosed[0];

    if tree.int_attr(profile_row, "AXDisclosureLevel")? != Some(1) {
        return Err(KaxError::blocked("self_room_not_verified"));
    }
    let disclosed_by = tree.elements_attr(profile_row, "AXDisclosedByRow")?;
    if disclosed_by.len() != 1 || !tree.same_element(disclosed_by[0], header_row) {
        return Err(KaxError::blocked("self_room_not_verified"));
    }

    let mut cells = Vec::new();
    for child in tree.elements_attr(profile_row, "AXChildren")? {
        if tree.string_attr(child, "AXRole", true)? == "AXCell" {
            cells.push(child);
        }
    }
    if cells.len() != 1 {
        return Err(KaxError::blocked("self_room_not_verified"));
    }
    let cell = cells[0];
    if !tree.action_names(cell).iter().any(|a| a == "AXShowMenu") {
        return Err(KaxError::blocked("self_room_not_verified"));
    }
    Ok(cell)
}

/// Select the unique enabled official self-chat menu item ("나와의 채팅" / "Chat with myself").
pub fn select_self_chat_menu_item<T: AxTree>(tree: &T, menus: &[usize]) -> Result<usize> {
    let mut items = Vec::new();
    for &menu in menus {
        for item in tree.elements_attr(menu, "AXChildren")? {
            if tree.string_attr(item, "AXRole", true)? == "AXMenuItem"
                && SELF_MENU_TITLES.contains(&tree.string_attr(item, "AXTitle", true)?.as_str())
                && tree.bool_attr(item, "AXEnabled")? == Some(true)
            {
                items.push(item);
            }
        }
    }
    if items.len() != 1 {
        return Err(KaxError::blocked("self_room_not_verified"));
    }
    Ok(items[0])
}

/// Pure production transition decision after pressing the official self-chat menu item.
///
/// Requires `focused_before` to be proven equal to `main_win` before the menu press.
/// Accepts ONLY:
/// 1. Exactly one newly opened and focused chat window (`already_open = false`), OR
/// 2. A proven `main_win -> existing_chat` focus transition (`already_open = true`).
/// Never accepts an unchanged focus or elapsed time alone.
pub fn resolve_self_menu_outcome<'a, W, F>(
    main_win: &W,
    focused_before: Option<&W>,
    before_chats: &[W],
    now_chats: &'a [W],
    focused_after: Option<&'a W>,
    same: F,
) -> Result<Option<(&'a W, bool)>>
where
    F: Fn(&W, &W) -> bool,
{
    let Some(fb) = focused_before else {
        return Err(KaxError::blocked("main_window_focus_not_observed"));
    };
    if !same(fb, main_win) {
        return Err(KaxError::blocked("main_window_focus_not_observed"));
    }

    let new_chats: Vec<&'a W> = now_chats
        .iter()
        .filter(|w| !same(w, main_win) && !before_chats.iter().any(|b| same(b, w)))
        .collect();
    if new_chats.len() > 1 {
        return Err(KaxError::blocked("target_room_window_ambiguous"));
    }
    if new_chats.len() == 1 {
        return Ok(focused_after.filter(|focus| same(focus, new_chats[0])).map(|_| (new_chats[0], false)));
    }
    if let Some(fa) = focused_after {
        if !same(fa, main_win) && now_chats.iter().any(|c| same(c, fa)) {
            return Ok(Some((fa, true)));
        }
    }
    Ok(None)
}

/// Verify the window opened or focused by the official self-chat menu against the requested target,
/// preserving exact-identity homonym friend rejection when a non-self window with `requested_title`
/// was already open.
pub fn verify_opened_self_window<T: AxTree>(
    tree: &T,
    requested_title: &str,
    existing_target: Option<usize>,
    candidate: usize,
) -> Result<String> {
    if tree.string_attr(candidate, "AXIdentifier", true)? == "Main Window" {
        return Err(KaxError::blocked("main_window_is_not_a_chat"));
    }
    let actual_title = tree.string_attr(candidate, "AXTitle", true)?;
    if actual_title.is_empty() {
        return Err(KaxError::blocked("self_room_not_verified"));
    }
    let mut same_title_count = 0usize;
    for w in tree.app_windows()? {
        if tree.string_attr(w, "AXTitle", true)? == actual_title {
            same_title_count += 1;
        }
    }
    if same_title_count != 1 {
        return Err(KaxError::blocked("target_room_window_ambiguous"));
    }
    if let Some(exp) = existing_target {
        if !tree.same_element(candidate, exp) || actual_title != requested_title {
            return Err(KaxError::blocked("self_room_not_verified"));
        }
        return Ok(actual_title);
    }
    if actual_title == requested_title || is_macos_self_alias(requested_title) {
        Ok(actual_title)
    } else {
        Err(KaxError::blocked("self_room_not_verified"))
    }
}

/// Verify that a dispatch target window still matches the verified self-chat AX element
/// bound during approval (`require_self = true`). Blocks if the window closed, became
/// ambiguous, or was replaced by a different AX element with the same title.
pub fn verify_bound_self_window<W, F>(
    matching_windows: &[W],
    expected_window: Option<&W>,
    same: F,
) -> Result<()>
where
    F: Fn(&W, &W) -> bool,
{
    let Some(expected) = expected_window else {
        return Err(KaxError::blocked("self_room_not_verified"));
    };
    if matching_windows.is_empty() {
        return Err(KaxError::blocked("target_room_window_not_open"));
    }
    if matching_windows.len() > 1 {
        return Err(KaxError::blocked("target_room_window_ambiguous"));
    }
    if !same(&matching_windows[0], expected) {
        return Err(KaxError::blocked("target_room_window_changed"));
    }
    Ok(())
}

/// Helper to build a JSON map of optional attribute failure counts.
pub fn optional_failures_json(map: &HashMap<String, usize>) -> Value {
    let mut out = serde_json::Map::new();
    for (k, v) in map {
        out.insert(k.clone(), json!(v));
    }
    Value::Object(out)
}
