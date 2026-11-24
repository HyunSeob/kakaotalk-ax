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

/// Verified 5-button search toolbar `AXIdentifier` layouts on macOS KakaoTalk:
/// - `e0127d0` baseline: `_NS:10, _NS:30, _NS:43, _NS:56, _NS:80`
/// - KakaoTalk Mac `26.10.0`: `_NS:10, _NS:31, _NS:45, _NS:73, _NS:85`
pub const KNOWN_SEARCH_TOOLBAR_LAYOUTS: &[&[&str; 5]] = &[
    &["_NS:10", "_NS:30", "_NS:43", "_NS:56", "_NS:80"],
    &["_NS:10", "_NS:31", "_NS:45", "_NS:73", "_NS:85"],
];

pub const SELF_PROFILE_LABELS: &[&str] = &["내 기본프로필", "My Default Profile", "My Profile"];
pub const SELF_MENU_TITLES: &[&str] = &["나와의 채팅", "Chat with myself"];

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
        json!({
            "name": self.name,
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
pub fn decide_room_open<T: AxTree>(
    tree: &T,
    entries: &[RoomEntry],
    title: &str,
    require_self: bool,
) -> Result<RoomLookupDecision> {
    if title.is_empty() {
        return Err(KaxError::blocked("exact_window_title_required"));
    }
    let matching: Vec<&RoomEntry> = entries.iter().filter(|e| e.name == title).collect();
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

/// Validate the 5 sibling search toolbar buttons against verified macOS KakaoTalk layouts.
pub fn validate_search_toolbar_buttons<T: AxTree>(
    tree: &T,
    field: usize,
    siblings: &[usize],
) -> Result<Vec<usize>> {
    let Some(pos) = siblings.iter().position(|&s| tree.same_element(s, field)) else {
        return Err(KaxError::blocked("search_field_missing"));
    };
    let buttons: Vec<usize> = siblings.iter().skip(pos + 1).take(5).copied().collect();
    if buttons.len() != 5 {
        return Err(KaxError::blocked("search_toolbar_layout_unknown"));
    }
    let mut ids = Vec::with_capacity(5);
    for &btn in &buttons {
        if tree.string_attr(btn, "AXRole", true)? != "AXButton" {
            return Err(KaxError::blocked("search_toolbar_layout_unknown"));
        }
        ids.push(tree.string_attr(btn, "AXIdentifier", true)?);
    }
    let matched = KNOWN_SEARCH_TOOLBAR_LAYOUTS.iter().any(|layout| {
        ids.iter()
            .zip(layout.iter())
            .all(|(actual, expected)| actual == expected)
    });
    if !matched {
        return Err(KaxError::blocked("search_toolbar_layout_unknown"));
    }
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
