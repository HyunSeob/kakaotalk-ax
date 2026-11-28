//! Windows KakaoTalk PC backend implementing all 14 `KakaoBackend` tools via `Win32Api`,
//! `ClipboardBackend`, and `OcrBackend`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::ax_core::{
    load_more_impl, search_messages_impl, LoadProbe, write_private_png, KakaoBackend, SendDriver, SendEngine,
};
use crate::error::{KaxError, Result};
use crate::macos_ax_logic::{MAX_MEMBER_LIMIT, MAX_ROOM_SCROLL_LIMIT};
use crate::state::{default_send_state_root, default_ui_root, sha256_hex, ui_session};
use crate::validation::{
    validate_integer, validate_output_path, validate_query_text, validate_snapshot_id_win32,
    validate_title, validate_win_text,
};
use crate::win32_api::{
    until, until_bool, OcrBackend, Win32Api, Window, VK_HOME, VK_PRIOR, VK_RETURN, WM_CLOSE,
};
use crate::win32_attachment::{viewport_key, PhotoLocator, PhotoLocatorHost};
use crate::win32_clipboard::{ClipboardBackend, ForegroundGuard};
use crate::win32_rows::{blank_chat, room_items, transcript_rows, RoomItem};

pub const WIN32_SOURCE: &str = "windows_win32";
pub const CLIPBOARD_SOURCE: &str = "windows_clipboard";
pub const OCR_SOURCE: &str = "windows_ocr";

const TOP_CLASSES: &[&str] = &["EVA_Window_Dblclk", "#32770"];
const LIST_CLASS: &str = "EVA_VH_ListControl_Dblclk";
const COMPOSER_CLASS: &str = "RICHEDIT50W";
const PLACEHOLDERS: &[&str] = &["메시지 입력", "Enter a message", "Type a message"];

fn tab_y_offset(tab_id: i32) -> Option<i32> {
    match tab_id {
        1149 => Some(26),
        1150 => Some(87),
        1075 => Some(143),
        _ => None,
    }
}

pub fn is_self_alias(value: &str) -> bool {
    let compact: String = value.chars().filter(|c| !c.is_whitespace()).collect();
    let lower = compact.to_lowercase();
    matches!(
        lower.as_str(),
        "나" | "나와의채팅" | "chatwithmyself" | "me"
    )
}

fn compact_ws(value: &str) -> String {
    value.chars().filter(|c| !c.is_whitespace()).collect()
}

fn response(status: &str, source: &str, extra: Map<String, Value>) -> Value {
    let mut out = Map::new();
    out.insert("status".to_string(), json!(status));
    out.insert("source".to_string(), json!(source));
    out.insert("databaseCalls".to_string(), json!(0));
    for (k, v) in extra {
        out.insert(k, v);
    }
    Value::Object(out)
}

fn blocked_response(source: &str, reason: &str) -> Value {
    let mut m = Map::new();
    m.insert("reason".to_string(), json!(reason));
    response("blocked", source, m)
}

pub fn composer_text_normalized(value: &str) -> String {
    value.replace("\r\n", "\n")
}

#[derive(Debug, Clone)]
pub struct AppInfo {
    pub hwnd: usize,
    pub pid: u32,
    pub created: u64,
    pub online: Option<usize>,
    pub locked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatInfo {
    pub hwnd: usize,
    pub name: String,
    pub pid: u32,
    pub created: u64,
    pub composer: usize,
    pub history: usize,
    pub search: Option<usize>,
}

impl ChatInfo {
    pub fn identity(&self) -> Vec<u64> {
        vec![
            self.pid as u64,
            self.created,
            self.hwnd as u64,
            self.composer as u64,
            self.history as u64,
        ]
    }
}

pub struct WindowsAx<A: Win32Api, C: ClipboardBackend<A>, O: OcrBackend> {
    pub api: Arc<A>,
    pub clipboard: Arc<C>,
    pub ocr: Arc<O>,
    pub ui_root: PathBuf,
    pub state_root: PathBuf,
    self_identity: Mutex<Option<Vec<u64>>>,
    search_views: Mutex<HashSet<[u8; 32]>>,
}

impl<A: Win32Api, C: ClipboardBackend<A>, O: OcrBackend> WindowsAx<A, C, O> {
    pub fn new(api: Arc<A>, clipboard: Arc<C>, ocr: Arc<O>) -> Self {
        Self::with_roots(
            api,
            clipboard,
            ocr,
            default_ui_root(),
            default_send_state_root(),
        )
    }

    pub fn with_roots(
        api: Arc<A>,
        clipboard: Arc<C>,
        ocr: Arc<O>,
        ui_root: PathBuf,
        state_root: PathBuf,
    ) -> Self {
        Self {
            api,
            clipboard,
            ocr,
            ui_root,
            state_root,
            self_identity: Mutex::new(None),
            search_views: Mutex::new(HashSet::new()),
        }
    }

    fn scale(&self, hwnd: usize, value: i32) -> i32 {
        let dpi = self.api.get_dpi_for_window(hwnd);
        let dpi = if dpi == 0 { 96 } else { dpi };
        ((value as f64) * (dpi as f64) / 96.0).round() as i32
    }

    fn top(&self, pid: u32) -> Result<Vec<Window>> {
        let mut out = Vec::new();
        for hwnd in self.api.handles(None) {
            if self.api.pid(hwnd) == pid
                && (self.api.is_window_visible(hwnd) || self.api.is_iconic(hwnd))
            {
                out.push(self.api.window(hwnd)?);
            }
        }
        Ok(out)
    }

    pub fn app(&self, ready: bool) -> Result<AppInfo> {
        let mut matches = Vec::new();
        let mut processes: HashMap<u32, (String, u64)> = HashMap::new();
        for hwnd in self.api.handles(None) {
            let cls = self.api.class_name(hwnd);
            if !TOP_CLASSES.contains(&cls.as_str()) {
                continue;
            }
            let name = self.api.title(hwnd)?;
            if name != "카카오톡" && name != "KakaoTalk" {
                continue;
            }
            let pid = self.api.pid(hwnd);
            let (exe, created) = processes
                .entry(pid)
                .or_insert_with(|| self.api.process(pid))
                .clone();
            if exe != "kakaotalk.exe" {
                continue;
            }
            let children = self.api.children(hwnd)?;
            let active: Vec<usize> = children
                .iter()
                .filter(|c| c.name.starts_with("OnlineMainView_") && c.control_id == 132)
                .map(|c| c.hwnd)
                .collect();
            let locked = children.iter().any(|c| {
                c.name.starts_with("LockModeView_")
                    && c.control_id == 183
                    && self.api.is_window_visible(c.hwnd)
            });
            matches.push(AppInfo {
                hwnd,
                pid,
                created,
                online: if active.len() == 1 {
                    Some(active[0])
                } else {
                    None
                },
                locked,
            });
        }
        if matches.is_empty() {
            return Err(KaxError::blocked("app_not_running"));
        }
        let primary: Vec<AppInfo> = matches
            .iter()
            .filter(|a| a.online.is_some() || a.locked)
            .cloned()
            .collect();
        let chosen = if primary.is_empty() { matches } else { primary };
        if chosen.len() != 1 {
            return Err(KaxError::blocked("app_window_ambiguous"));
        }
        let app = chosen.into_iter().next().unwrap();
        if ready {
            self.check_ready(&app)?;
        }
        Ok(app)
    }

    fn check_ready(&self, app: &AppInfo) -> Result<()> {
        if app.locked {
            return Err(KaxError::blocked("lock_mode_active"));
        }
        if app.online.is_none() {
            return Err(KaxError::blocked("not_logged_in"));
        }
        self.check_modal(app)
    }

    // KakaoTalk draws the promotional balloon as a childless, layered popup
    // inside its main window. Ordinary EVA dialogs also use an empty title.
    fn is_promo_balloon(&self, hwnd: usize, owner: usize) -> bool {
        if !self.api.has_balloon_styles(hwnd) || self.api.get_foreground_window() == hwnd {
            return false;
        }
        let (Ok((left, top, right, bottom)), Ok((owner_left, owner_top, owner_right, owner_bottom))) =
            (self.api.rect(hwnd, false), self.api.rect(owner, false))
        else {
            return false;
        };
        right - left == self.scale(owner, 156)
            && bottom - top == self.scale(owner, 44)
            && left >= owner_left
            && top >= owner_top
            && right <= owner_right
            && bottom <= owner_bottom
    }

    fn check_modal(&self, app: &AppInfo) -> Result<()> {
        if !self.api.is_window_enabled(app.hwnd) {
            return Err(KaxError::blocked("modal_dialog_present"));
        }
        for w in self.top(app.pid)? {
            if w.hwnd == app.hwnd || !self.api.is_window_visible(w.hwnd) {
                continue;
            }
            if !self.api.is_window_enabled(w.hwnd) {
                return Err(KaxError::blocked("modal_dialog_present"));
            }
            let owner = self.api.get_owner(w.hwnd);
            if matches!(w.cls.as_str(), "tooltips_class32" | "SysShadow") {
                continue;
            }
            if TOP_CLASSES.contains(&w.cls.as_str()) && self.chat(&w, app)?.is_some() {
                continue;
            }
            if owner == app.hwnd && w.cls == "EVA_Window_Dblclk" && w.name.is_empty() {
                let children = self.api.children(w.hwnd)?;
                let ids: HashSet<i32> = children.iter().map(|c| c.control_id).collect();
                let is_ad = [5101, 5102, 5103, 5000].iter().all(|id| ids.contains(id))
                    && children.iter().any(|c| c.cls == "Chrome_WidgetWin_0");
                if is_ad || (children.is_empty() && self.is_promo_balloon(w.hwnd, app.hwnd)) {
                    continue;
                }
            }
            if owner != 0
                || matches!(
                    w.cls.as_str(),
                    "#32770" | "EVA_Menu" | "EVA_Window_Dblclk"
                )
            {
                return Err(KaxError::blocked("modal_dialog_present"));
            }
        }
        Ok(())
    }

    fn chat(&self, window: &Window, app: &AppInfo) -> Result<Option<ChatInfo>> {
        if window.hwnd == app.hwnd
            || !TOP_CLASSES.contains(&window.cls.as_str())
            || window.name.is_empty()
        {
            return Ok(None);
        }
        let controls = self.api.children(window.hwnd)?;
        let drafts: Vec<usize> = controls
            .iter()
            .filter(|c| c.cls == COMPOSER_CLASS)
            .map(|c| c.hwnd)
            .collect();
        let histories: Vec<usize> = controls
            .iter()
            .filter(|c| {
                c.cls == LIST_CLASS
                    && c.control_id == 100
                    && self.api.get_parent(c.hwnd) == window.hwnd
            })
            .map(|c| c.hwnd)
            .collect();
        let searches: Vec<usize> = controls
            .iter()
            .filter(|c| {
                c.cls == "Edit"
                    && c.control_id == 100
                    && self.api.get_parent(c.hwnd) == window.hwnd
            })
            .map(|c| c.hwnd)
            .collect();
        if drafts.len() == 1 && histories.len() == 1 {
            return Ok(Some(ChatInfo {
                hwnd: window.hwnd,
                name: window.name.clone(),
                pid: app.pid,
                created: app.created,
                composer: drafts[0],
                history: histories[0],
                search: if searches.len() == 1 {
                    Some(searches[0])
                } else {
                    None
                },
            }));
        }
        Ok(None)
    }

    fn chats(&self, app: &AppInfo) -> Result<Vec<ChatInfo>> {
        let mut out = Vec::new();
        for w in self.top(app.pid)? {
            if let Some(c) = self.chat(&w, app)? {
                out.push(c);
            }
        }
        Ok(out)
    }

    pub fn exact_chat(&self, app: &AppInfo, name: &str, allow_missing: bool) -> Result<Option<ChatInfo>> {
        let matches: Vec<ChatInfo> = self
            .chats(app)?
            .into_iter()
            .filter(|c| c.name == name)
            .collect();
        if matches.len() > 1 {
            return Err(KaxError::blocked("target_room_window_ambiguous"));
        }
        if matches.is_empty() {
            if allow_missing {
                return Ok(None);
            }
            return Err(KaxError::blocked("target_room_window_not_open"));
        }
        Ok(matches.into_iter().next())
    }

    fn check_chat(&self, expected: &ChatInfo, modal: bool) -> Result<ChatInfo> {
        let app = self.app(modal)?;
        let actual = self.exact_chat(&app, &expected.name, false)?.unwrap();
        if actual.identity() != expected.identity() || self.api.title(expected.hwnd)? != expected.name
        {
            return Err(KaxError::blocked("target_room_window_changed"));
        }
        Ok(actual)
    }

    fn tab(&self, app: &AppInfo, tab_id: i32) -> Result<usize> {
        if !self.api.is_window_visible(app.hwnd) || self.api.is_iconic(app.hwnd) {
            return Err(KaxError::blocked("main_window_not_visible"));
        }
        let online = app.online.ok_or_else(|| KaxError::blocked("not_logged_in"))?;
        let tabs: Vec<usize> = self
            .api
            .children(online)?
            .into_iter()
            .filter(|c| c.control_id == tab_id && self.api.get_parent(c.hwnd) == online)
            .map(|c| c.hwnd)
            .collect();
        if tabs.len() != 1 {
            return Err(KaxError::blocked("main_tab_unavailable"));
        }
        if !self.api.is_window_visible(tabs[0]) {
            let y_off = tab_y_offset(tab_id)
                .ok_or_else(|| KaxError::blocked("main_tab_unavailable"))?;
            self.api.click(
                online,
                self.scale(app.hwnd, 31),
                self.scale(app.hwnd, y_off),
                false,
                false,
            )?;
            if !until_bool(|| self.api.is_window_visible(tabs[0]), 0.7, 0.025) {
                return Err(KaxError::blocked("main_tab_not_observed"));
            }
        }
        Ok(tabs[0])
    }

    fn room_controls(&self, app: &AppInfo) -> Result<(usize, usize, usize)> {
        let view = self.tab(app, 1150)?;
        let children = self.api.children(view)?;
        let edit: Vec<usize> = children
            .iter()
            .filter(|c| c.cls == "Edit" && c.control_id == 100)
            .map(|c| c.hwnd)
            .collect();
        let listing: Vec<usize> = children
            .iter()
            .filter(|c| c.name.starts_with("ChatRoomListCtrl_") && c.control_id == 1003)
            .map(|c| c.hwnd)
            .collect();
        let search: Vec<usize> = children
            .iter()
            .filter(|c| c.name.starts_with("SearchListCtrl_") && c.control_id == 1003)
            .map(|c| c.hwnd)
            .collect();
        if edit.len() != 1 || listing.len() != 1 || search.len() != 1 {
            return Err(KaxError::blocked("room_list_controls_unavailable"));
        }
        Ok((edit[0], listing[0], search[0]))
    }

    fn room_query(&self, controls: (usize, usize, usize), query: &str) -> Result<usize> {
        let (edit, default_list, search_list) = controls;
        self.api.set_text(edit, query)?;
        let desired = if query.is_empty() {
            default_list
        } else {
            search_list
        };
        let observed = until_bool(
            || {
                self.api.control_text(edit).ok().as_deref() == Some(query)
                    && self.api.is_window_visible(desired)
            },
            0.9,
            0.025,
        );
        if !observed {
            return Err(KaxError::blocked("room_search_not_observed"));
        }
        std::thread::sleep(Duration::from_millis(120));
        Ok(desired)
    }

    fn list_rows(&self, hwnd: usize) -> Result<Vec<RoomItem>> {
        let frame = self.api.capture(hwnd)?;
        let dpi = self.api.get_dpi_for_window(hwnd);
        let dpi = if dpi == 0 { 96 } else { dpi };
        let mut ocr_err: Option<KaxError> = None;
        let items = room_items(
            &frame,
            |crop| match self.ocr.recognize("ko", crop) {
                Ok(lines) => lines,
                Err(e) => {
                    if ocr_err.is_none() {
                        ocr_err = Some(e);
                    }
                    Vec::new()
                }
            },
            dpi,
        );
        if let Some(err) = ocr_err {
            return Err(err);
        }
        Ok(items)
    }

    fn room_output(items: &[RoomItem], query: &str, search_query: &str, total_exposed: usize) -> Value {
        let mut extra = Map::new();
        extra.insert("textExact".to_string(), json!(false));
        extra.insert("query".to_string(), json!(query));
        extra.insert("searchQuery".to_string(), json!(search_query));
        extra.insert("scope".to_string(), json!("currently_visible_room_list"));
        extra.insert("total".to_string(), json!(items.len()));
        extra.insert("totalExposed".to_string(), json!(total_exposed));
        extra.insert("reportedRowCount".to_string(), Value::Null);
        extra.insert(
            "items".to_string(),
            Value::Array(items.iter().map(RoomItem::to_public_json).collect()),
        );
        response("ok", OCR_SOURCE, extra)
    }

    fn switch_foreground(&self, hwnd: usize, allowed_ticks: &mut HashSet<u32>) -> bool {
        if self.api.get_foreground_window() != hwnd {
            self.api.set_foreground_window(hwnd);
        }
        if self.api.get_foreground_window() != hwnd {
            self.api
                .activate_foreground(hwnd, Some(allowed_ticks), 0.6);
        }
        until_bool(|| self.api.get_foreground_window() == hwnd, 0.4, 0.025)
    }

    fn restore_room_foreground(
        &self,
        app: &AppInfo,
        original_fg: usize,
        allowed_ticks: &mut HashSet<u32>,
    ) -> Result<()> {
        // Restore foreground if we still own it and user did not interact.
        let current_fg = self.api.get_foreground_window();
        if current_fg != 0
            && current_fg != original_fg
            && self.api.pid(current_fg) == app.pid
            && self
                .api
                .last_input()
                .ok()
                .is_some_and(|t| allowed_ticks.contains(&t))
            && original_fg != 0
            && self.api.is_window(original_fg)
            && !self.switch_foreground(original_fg, allowed_ticks)
        {
            return Err(KaxError::blocked("foreground_restore_failed"));
        }
        Ok(())
    }

    pub fn verify_self(
        &self,
        app: &AppInfo,
        name: Option<&str>,
        expected: Option<&ChatInfo>,
    ) -> Result<(ChatInfo, bool)> {
        {
            let cached_id = self.self_identity.lock().unwrap().clone();
            if let (Some(exp), Some(ref cid)) = (expected, &cached_id) {
                if &exp.identity() == cid {
                    return Ok((exp.clone(), true));
                }
            }
            if name.is_none() && expected.is_none() {
                if let Some(ref cid) = cached_id {
                    let cached: Vec<ChatInfo> = self
                        .chats(app)?
                        .into_iter()
                        .filter(|c| &c.identity() == cid)
                        .collect();
                    if cached.len() == 1 {
                        return Ok((self.check_chat(&cached[0], true)?, true));
                    }
                }
            }
        }

        let online = app.online.ok_or_else(|| KaxError::blocked("not_logged_in"))?;
        let initial_tab = self
            .api
            .children(online)?
            .into_iter()
            .find(|c| tab_y_offset(c.control_id).is_some() && self.api.is_window_visible(c.hwnd))
            .map(|c| c.control_id);

        let original_fg = self.api.get_foreground_window();
        let mut allowed_ticks = HashSet::new();
        allowed_ticks.insert(self.api.idle(400)?);

        let check_no_input = |ticks: &HashSet<u32>| -> Result<()> {
            if !ticks.contains(&self.api.last_input()?) {
                return Err(KaxError::blocked("user_input_active"));
            }
            Ok(())
        };

        let result: Result<(ChatInfo, bool)> = (|| {
            if !self.api.is_window_visible(app.hwnd) || self.api.is_iconic(app.hwnd) {
                return Err(KaxError::blocked("main_window_not_visible"));
            }
            let existing_chats = self.chats(app)?;
            let before: HashSet<usize> = existing_chats.iter().map(|c| c.hwnd).collect();
            let before_iconic: HashSet<usize> = existing_chats
                .iter()
                .filter(|c| self.api.is_iconic(c.hwnd))
                .map(|c| c.hwnd)
                .collect();

            if !self.switch_foreground(app.hwnd, &mut allowed_ticks) {
                return Err(KaxError::blocked("foreground_unavailable"));
            }
            check_no_input(&allowed_ticks)?;
            let view = self.tab(app, 1149)?;
            let candidates: Vec<usize> = self
                .api
                .children(view)?
                .into_iter()
                .filter(|c| {
                    c.name.starts_with("ContactListCtrl_") && self.api.is_window_visible(c.hwnd)
                })
                .map(|c| c.hwnd)
                .collect();
            if candidates.len() != 1 {
                return Err(KaxError::blocked("self_chat_unverified"));
            }

            let mut menu_hwnd: Option<usize> = None;
            let mut created: Option<ChatInfo> = None;

            let inner: Result<(ChatInfo, bool)> = (|| {
                check_no_input(&allowed_ticks)?;
                self.api.click(
                    candidates[0],
                    self.scale(app.hwnd, 104),
                    self.scale(app.hwnd, 26),
                    false,
                    true,
                )?;
                let menu = until(
                    || {
                        self.top(app.pid)
                            .ok()?
                            .into_iter()
                            .find(|w| {
                                w.cls == "EVA_Menu" && self.api.get_owner(w.hwnd) == app.hwnd
                            })
                            .map(|w| w.hwnd)
                    },
                    0.7,
                    0.025,
                )
                .ok_or_else(|| KaxError::blocked("self_chat_unverified"))?;
                menu_hwnd = Some(menu);

                let frame = until(
                    || match self.api.capture(menu) {
                        Ok(f) => Some(f),
                        Err(KaxError::Blocked(ref r))
                            if r == "window_capture_blank" || r == "window_capture_failed" =>
                        {
                            None
                        }
                        Err(_) => None,
                    },
                    0.55,
                    0.025,
                )
                .ok_or_else(|| KaxError::blocked("self_chat_unverified"))?;

                let lines = self.ocr.recognize("ko", &frame.scale(2))?;
                let labels: HashSet<String> = lines.iter().map(|l| compact_ws(&l.text)).collect();
                let max_y = self.scale(menu, 30) as f64;
                let self_items: Vec<_> = lines
                    .iter()
                    .filter(|l| {
                        matches!(
                            compact_ws(&l.text).as_str(),
                            "나와의" | "나와의채팅" | "Chatwithmyself"
                        ) && (l.y / 2.0) < max_y
                    })
                    .collect();
                if self_items.len() != 1
                    || !labels.contains("프로필보기")
                    || !labels.contains("프로필편집")
                    || !self.api.is_window_visible(menu)
                {
                    return Err(KaxError::blocked("self_chat_unverified"));
                }

                check_no_input(&allowed_ticks)?;
                let prior = self.api.get_foreground_window();
                self.api.click(
                    menu,
                    self.scale(menu, 30),
                    self.scale(menu, 15),
                    false,
                    false,
                )?;

                let observed = until(
                    || {
                        if self.api.is_window_visible(menu) {
                            return None;
                        }
                        let active = self.api.get_foreground_window();
                        let chats = self.chats(app).ok()?;
                        let new_chats: Vec<ChatInfo> = chats
                            .iter()
                            .filter(|c| !before.contains(&c.hwnd))
                            .cloned()
                            .collect();
                        if new_chats.len() == 1 {
                            return Some(new_chats.into_iter().next().unwrap());
                        }
                        if new_chats.is_empty() {
                            if active != prior {
                                if let Some(found) = chats.iter().find(|c| c.hwnd == active) {
                                    return Some(found.clone());
                                }
                            }
                            let unmin: Vec<ChatInfo> = chats
                                .into_iter()
                                .filter(|c| {
                                    before_iconic.contains(&c.hwnd) && !self.api.is_iconic(c.hwnd)
                                })
                                .collect();
                            if unmin.len() == 1 {
                                return Some(unmin.into_iter().next().unwrap());
                            }
                        }
                        None
                    },
                    1.5,
                    0.025,
                )
                .ok_or_else(|| KaxError::blocked("self_chat_unverified"))?;

                if !before.contains(&observed.hwnd) {
                    created = Some(observed.clone());
                }
                check_no_input(&allowed_ticks)?;
                if name.is_some_and(|n| observed.name != n)
                    || expected.is_some_and(|e| observed.identity() != e.identity())
                {
                    return Err(KaxError::blocked("self_chat_unverified"));
                }
                let chat = self.check_chat(&observed, true)?;
                created = None;
                let already = before.contains(&chat.hwnd);
                Ok((chat, already))
            })();

            if let Some(menu) = menu_hwnd {
                if self.api.is_window_visible(menu) {
                    let _ = self.api.post(menu, WM_CLOSE, 0, 0);
                    let _ = until_bool(|| !self.api.is_window_visible(menu), 0.3, 0.025);
                }
            }
            if let Ok(()) = check_no_input(&allowed_ticks) {
                if let Some(c) = created {
                    let _ = self.close_chat(&c);
                }
                if let Some(tab_id) = initial_tab {
                    if tab_id != 1149 {
                        let _ = self.tab(app, tab_id);
                    }
                }
            }
            inner
        })();

        self.restore_room_foreground(app, original_fg, &mut allowed_ticks)?;
        if let Ok((chat, _)) = &result {
            *self.self_identity.lock().unwrap() = Some(chat.identity());
        }
        result
    }

    pub fn composer_frame_empty(&self, chat: &ChatInfo) -> Result<bool> {
        let frame = self.api.capture(chat.hwnd)?;
        let s = |n: i32| self.scale(chat.hwnd, n) as usize;
        if frame.width < s(210) || frame.height < s(140) {
            return Ok(false);
        }
        // Sample pure button-fill margins clear of glyphs and split-button divider.
        for (dx, dy) in [(52, 13), (40, 34), (15, 22)] {
            let (b, g, r) = frame.pixel(frame.width - s(dx), frame.height - s(dy));
            let mx = b.max(g).max(r);
            let mn = b.min(g).min(r);
            if !((232..=250).contains(&b)
                && (232..=250).contains(&g)
                && (232..=250).contains(&r)
                && (mx - mn) <= 8)
            {
                return Ok(false);
            }
        }
        let (left, top, _, _) = self.api.rect(chat.hwnd, false)?;
        let (cx, cy, _, _) = self.api.rect(chat.composer, false)?;
        let x = cx - left;
        let y = cy - top;
        if x < 0
            || y < 0
            || (x as usize) >= frame.width.saturating_sub(s(138))
            || (y as usize) >= frame.height.saturating_sub(s(23))
        {
            return Ok(false);
        }
        let x = x as usize;
        let y = y as usize;
        let mut min_ink = 255u8;
        let mut any_px = false;
        for line in (y + s(3))..(y + s(22)) {
            for col in (x + s(4))..(x + s(135)) {
                let (b, g, r) = frame.pixel(col, line);
                let mn = b.min(g).min(r);
                if mn < min_ink {
                    min_ink = mn;
                }
                any_px = true;
            }
        }
        Ok(any_px && (100..220).contains(&min_ink))
    }

    pub fn composer_empty(&self, chat: &ChatInfo) -> Result<bool> {
        let value = self.api.control_text(chat.composer)?;
        if value.is_empty() {
            return Ok(true);
        }
        if !PLACEHOLDERS.contains(&value.as_str()) {
            return Ok(false);
        }
        if self.api.is_iconic(chat.hwnd) {
            return ForegroundGuard::with_activated(self.api.as_ref(), chat.hwnd, |_guard| {
                let min_w = self.scale(chat.hwnd, 210);
                let _ = until_bool(
                    || {
                        self.api
                            .rect(chat.hwnd, false)
                            .map(|r| r.2 - r.0 >= min_w)
                            .unwrap_or(false)
                    },
                    0.4,
                    0.025,
                );
                if self.api.control_text(chat.composer)? != value {
                    return Ok(false);
                }
                self.composer_frame_empty(chat)
            });
        }
        self.composer_frame_empty(chat)
    }

    pub fn send_button_ready(&self, chat: &ChatInfo) -> Result<bool> {
        let frame = self.api.capture(chat.hwnd)?;
        let s = |n: i32| self.scale(chat.hwnd, n) as usize;
        if frame.width < s(70) || frame.height < s(40) {
            return Ok(false);
        }
        for (dx, dy) in [(52, 13), (40, 34), (15, 22)] {
            let (b, g, r) = frame.pixel(frame.width - s(dx), frame.height - s(dy));
            if !(r >= 200
                && g >= 160
                && (r as i32 - b as i32) >= 110
                && (g as i32 - b as i32) >= 100)
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn close_chat(&self, chat: &ChatInfo) -> Result<()> {
        self.check_chat(chat, true)?;
        let before_text = self.api.control_text(chat.composer)?;
        if !self.composer_empty(chat)? {
            return Err(KaxError::blocked("existing_draft_preserved"));
        }
        let input_at = self.api.idle(400)?;
        self.check_chat(chat, true)?;
        if self.api.is_iconic(chat.hwnd) && PLACEHOLDERS.contains(&before_text.as_str()) {
            if self.api.control_text(chat.composer)? != before_text {
                return Err(KaxError::blocked("existing_draft_preserved"));
            }
        } else if !self.composer_empty(chat)? {
            return Err(KaxError::blocked("existing_draft_preserved"));
        }
        if self.api.last_input()? != input_at {
            return Err(KaxError::blocked("user_input_active"));
        }
        self.api.post(chat.hwnd, WM_CLOSE, 0, 0)?;
        if !until_bool(
            || !self.api.is_window(chat.hwnd) || !self.api.is_window_visible(chat.hwnd),
            0.7,
            0.025,
        ) {
            return Err(KaxError::blocked("room_close_not_observed"));
        }
        let mut sid = self.self_identity.lock().unwrap();
        if sid.as_ref() == Some(&chat.identity()) {
            *sid = None;
        }
        Ok(())
    }

    fn arm_history(&self, chat: &ChatInfo, guard: &ForegroundGuard<'_, A>) -> Result<()> {
        guard.check()?;
        self.api.arm_list(chat.history)?;
        guard.focus(chat.history)
    }

    fn after_copy(&self, chat: &ChatInfo, guard: &ForegroundGuard<'_, A>) -> Result<()> {
        if self.api.is_window(chat.history) && self.api.get_foreground_window() == chat.hwnd {
            guard.check()?;
            self.api.clear_selection(chat.history)?;
        }
        Ok(())
    }

    fn copied(
        &self,
        chat: &ChatInfo,
        guard: &mut ForegroundGuard<'_, A>,
        select_all: bool,
    ) -> Result<String> {
        if select_all {
            self.arm_history(chat, guard)?;
        } else {
            guard.focus(chat.history)?;
        }
        let mut sent = false;
        let history = chat.history;
        let mut perform = |g: &mut ForegroundGuard<'_, A>| -> Result<()> {
            if select_all {
                g.keys(history, &[65], true)?;
                std::thread::sleep(Duration::from_millis(25));
                g.keys(history, &[67], true)?;
            } else {
                g.keys(history, &[67], true)?;
            }
            sent = true;
            Ok(())
        };
        let mut check_target = || -> Result<()> {
            self.check_chat(chat, false)?;
            Ok(())
        };
        let res = self
            .clipboard
            .copy_chat(chat.pid, &mut perform, guard, &mut check_target);
        if sent && select_all {
            let _ = self.after_copy(chat, guard);
        }
        res
    }

    pub fn snapshot(
        &self,
        chat: &ChatInfo,
        guard: Option<&mut ForegroundGuard<'_, A>>,
    ) -> Result<(String, Vec<Value>, f64)> {
        let started = Instant::now();
        let read_with = |g: &mut ForegroundGuard<'_, A>| -> Result<String> {
            match self.copied(chat, g, true) {
                Ok(t) => Ok(t),
                Err(KaxError::Blocked(ref r)) if r == "clipboard_copy_not_observed" => {
                    let history_frame = self.api.capture(chat.history)?;
                    if blank_chat(&history_frame) {
                        self.check_chat(chat, false)?;
                        g.check()?;
                        return Ok(String::new());
                    }
                    if history_frame.height >= 16 {
                        g.click(chat.history, 4, history_frame.height as i32 - 8)?;
                        return self.copied(chat, g, true);
                    }
                    Err(KaxError::blocked("clipboard_copy_not_observed"))
                }
                Err(e) => Err(e),
            }
        };

        let text = if let Some(g) = guard {
            read_with(g)?
        } else {
            ForegroundGuard::with_activated(self.api.as_ref(), chat.hwnd, |g| read_with(g))?
        };
        let rows = transcript_rows(&text);
        let snapshot_id = crate::ax_core::legacy_snapshot_id(&json!([chat.name, chat.identity(), text]));
        let elapsed = (started.elapsed().as_secs_f64() * 1000.0 * 10.0).round() / 10.0;
        Ok((snapshot_id, rows, elapsed))
    }

    fn search_active(&self, chat: &ChatInfo) -> bool {
        let Some(search) = chat.search else {
            return false;
        };
        if !self.api.is_window_visible(search) {
            return false;
        }
        let Ok((x, y, right, bottom)) = self.api.rect(search, false) else {
            return false;
        };
        let Ok((_, list_top, _, _)) = self.api.rect(chat.history, false) else {
            return false;
        };
        right > x && bottom > y && list_top >= bottom
    }

    fn navigation_position(&self, chat: &ChatInfo) -> Result<[u8; 32]> {
        viewport_key(&self.api.capture(chat.history)?)
    }

    fn search_bar(&self, chat: &ChatInfo, guard: &mut ForegroundGuard<'_, A>) -> Result<()> {
        if self.search_active(chat) {
            return Ok(());
        }
        self.arm_history(chat, guard)?;
        guard.keys(chat.history, &[70], true)?;
        if !until_bool(|| self.search_active(chat), 0.2, 0.025) {
            let (_, _, width, _) = self.api.rect(chat.hwnd, true)?;
            guard.check()?;
            self.api.click(
                chat.hwnd,
                width - self.scale(chat.hwnd, 81),
                self.scale(chat.hwnd, 58),
                false,
                false,
            )?;
            if !until_bool(|| self.search_active(chat), 0.5, 0.025) {
                return Err(KaxError::blocked("message_search_bar_unavailable"));
            }
        }
        Ok(())
    }

    fn search_command(&self, chat: &ChatInfo, action: &str, query: &str) -> Result<Value> {
        ForegroundGuard::with_activated(self.api.as_ref(), chat.hwnd, |guard| {
            self.check_chat(chat, true)?;
            let search_hwnd = chat
                .search
                .ok_or_else(|| KaxError::blocked("message_search_bar_unavailable"))?;

            let verified = if action == "search_query" {
                self.search_views.lock().unwrap().clear();
                self.search_bar(chat, guard)?;
                guard.focus(search_hwnd)?;
                let before = self.navigation_position(chat)?;
                self.api.set_text(search_hwnd, query)?;
                if self.api.control_text(search_hwnd)? != query {
                    return Err(KaxError::blocked("search_query_changed"));
                }
                guard.keys(search_hwnd, &[VK_RETURN], false)?;
                std::thread::sleep(Duration::from_millis(200));
                let (sl, _, sr, _) = self.api.rect(search_hwnd, false)?;
                let width = sr - sl;
                let after = self.navigation_position(chat)?;
                let v = self.search_active(chat)
                    && (after != before || width <= self.scale(chat.hwnd, 105));
                self.search_views.lock().unwrap().insert(after);
                v
            } else {
                if !self.search_active(chat) || self.api.control_text(search_hwnd)? != query {
                    return Err(KaxError::blocked("search_query_changed"));
                }
                let (_, top, right, bottom) = self.api.rect(search_hwnd, false)?;
                let origin = self.api.point(chat.hwnd, right, (top + bottom) / 2, false)?;
                let x = origin.0 + self.scale(chat.hwnd, 39);
                let y = origin.1;
                let (_, _, width, height) = self.api.rect(chat.hwnd, true)?;
                if !(x > 0 && x < width - self.scale(chat.hwnd, 55) && y > 0 && y < height) {
                    return Err(KaxError::blocked("search_navigation_unavailable"));
                }
                let prior_view = self.navigation_position(chat)?;
                guard.click(chat.hwnd, x, y)?;
                let after = until(
                    || {
                        let view = self.navigation_position(chat).ok()?;
                        if view != prior_view {
                            Some(view)
                        } else {
                            None
                        }
                    },
                    0.65,
                    0.09,
                );
                guard.check()?;
                let mut views = self.search_views.lock().unwrap();
                let exhausted = match after {
                    None => true,
                    Some(v) => !views.insert(v),
                };
                if exhausted {
                    let mut m = Map::new();
                    m.insert("windowTitle".to_string(), json!(chat.name));
                    m.insert("searchOutcome".to_string(), json!("navigation_exhausted"));
                    m.insert("searchVerified".to_string(), json!(true));
                    m.insert("searchState".to_string(), json!({"previousEnabled": false}));
                    return Ok(response("ok", WIN32_SOURCE, m));
                }
                self.search_active(chat) && self.api.control_text(search_hwnd)? == query
            };
            guard.check()?;
            let mut m = Map::new();
            m.insert("windowTitle".to_string(), json!(chat.name));
            m.insert("searchVerified".to_string(), json!(verified));
            if !verified {
                m.insert("reason".to_string(), json!("search_result_not_observed"));
            }
            Ok(response(if verified { "ok" } else { "partial" }, WIN32_SOURCE, m))
        })
    }

    fn control_step(&self, action: &str, window_title: &str, query: &str) -> Result<Value> {
        let res: Result<Value> = (|| {
            let chat = self.exact_chat(&self.app(true)?, window_title, false)?.unwrap();
            if action == "load_older" {
                return ForegroundGuard::with_activated(self.api.as_ref(), chat.hwnd, |guard| {
                    self.arm_history(&chat, guard)?;
                    guard.keys(chat.history, &[VK_HOME, VK_PRIOR], false)?;
                    guard.check()?;
                    self.api.wheel(chat.history, 360)?;
                    std::thread::sleep(Duration::from_millis(230));
                    guard.check()?;
                    let mut m = Map::new();
                    m.insert("windowTitle".to_string(), json!(chat.name));
                    m.insert("scrollRequested".to_string(), json!(true));
                    Ok(response("ok", WIN32_SOURCE, m))
                });
            }
            self.search_command(&chat, action, query)
        })();
        match res {
            Ok(v) => Ok(v),
            Err(KaxError::Blocked(r)) => Ok(blocked_response(WIN32_SOURCE, &r)),
            Err(e) => Err(e),
        }
    }

    fn run_desktop<F>(&self, source: &str, f: F) -> Result<Value>
    where
        F: FnOnce() -> Result<Value>,
    {
        let _ui = ui_session(&self.ui_root)?;
        let _dpi = self.api.dpi_aware();
        match f() {
            Ok(v) => Ok(v),
            Err(KaxError::Blocked(r)) => Ok(blocked_response(source, &r)),
            Err(e) => Err(e),
        }
    }
}

struct WinPhotoHost<'a, A: Win32Api, C: ClipboardBackend<A>, O: OcrBackend> {
    win: &'a WindowsAx<A, C, O>,
    chat: &'a ChatInfo,
}

impl<'a, A: Win32Api, C: ClipboardBackend<A>, O: OcrBackend> PhotoLocatorHost<A>
    for WinPhotoHost<'a, A, C, O>
{
    fn api(&self) -> &A {
        self.win.api.as_ref()
    }
    fn chat_hwnd(&self) -> usize {
        self.chat.hwnd
    }
    fn history_hwnd(&self) -> usize {
        self.chat.history
    }
    fn arm_history(&self, guard: &mut ForegroundGuard<'_, A>) -> Result<()> {
        self.win.arm_history(self.chat, guard)
    }
    fn after_copy(&self, guard: &mut ForegroundGuard<'_, A>) -> Result<()> {
        self.win.after_copy(self.chat, guard)
    }
    fn copied_selection(&self, guard: &mut ForegroundGuard<'_, A>) -> Result<String> {
        self.win.copied(self.chat, guard, false)
    }
}

struct WindowsSendAdapter<'a, A: Win32Api, C: ClipboardBackend<A>, O: OcrBackend> {
    win: &'a WindowsAx<A, C, O>,
}

impl<'a, A: Win32Api, C: ClipboardBackend<A>, O: OcrBackend> WindowsSendAdapter<'a, A, C, O> {
    fn bound_chat(&self, record: &Map<String, Value>) -> Result<ChatInfo> {
        let title = record
            .get("windowTitle")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let chat = self.win.exact_chat(&self.win.app(true)?, title, false)?.unwrap();
        let expected_id: Option<Vec<u64>> = record
            .get("windowIdentity")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|x| x.as_u64()).collect());
        if expected_id.as_ref() != Some(&chat.identity()) {
            return Err(KaxError::blocked("target_room_window_changed"));
        }
        Ok(chat)
    }
}

impl<'a, A: Win32Api, C: ClipboardBackend<A>, O: OcrBackend> SendDriver
    for WindowsSendAdapter<'a, A, C, O>
{
    fn source(&self) -> &'static str {
        WIN32_SOURCE
    }

    fn confirmation_source(&self) -> &'static str {
        CLIPBOARD_SOURCE
    }

    fn unknown_reason(&self) -> &'static str {
        "windows_confirmation_not_established"
    }

    fn ui_root(&self) -> &Path {
        &self.win.ui_root
    }

    fn state_root(&self) -> &Path {
        &self.win.state_root
    }

    fn text_hash(&self, text: &str) -> String {
        sha256_hex(&composer_text_normalized(text))
    }

    fn body_hashes(&self, read_result: &Value) -> Vec<String> {
        read_result
            .get("items")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter(|r| r.get("kind").and_then(|v| v.as_str()) == Some("text"))
                    .filter_map(|r| r.get("text").and_then(|v| v.as_str()))
                    .map(|t| self.text_hash(t))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn prepare(&self, window_title: &str, require_self: bool) -> Result<(Value, Map<String, Value>)> {
        let res: Result<(Value, Map<String, Value>)> = (|| {
            let app = self.win.app(true)?;
            let chat = self.win.exact_chat(&app, window_title, false)?.unwrap();
            if !self.win.composer_empty(&chat)? {
                return Err(KaxError::blocked("existing_draft_preserved"));
            }
            if require_self {
                self.win.verify_self(&app, Some(window_title), Some(&chat))?;
            }
            let mut m = Map::new();
            m.insert("windowTitle".to_string(), json!(chat.name));
            let mut target = Map::new();
            target.insert("windowIdentity".to_string(), json!(chat.identity()));
            Ok((response("ok", WIN32_SOURCE, m), target))
        })();
        match res {
            Ok(v) => Ok(v),
            Err(KaxError::Blocked(r)) => Ok((blocked_response(WIN32_SOURCE, &r), Map::new())),
            Err(e) => Err(e),
        }
    }

    fn dispatch(&self, record: &Map<String, Value>, text: &str) -> Result<Value> {
        let mut written = false;
        let mut attempted = false;
        let res: Result<Value> = (|| {
            let chat = self.bound_chat(record)?;
            ForegroundGuard::with_activated(self.win.api.as_ref(), chat.hwnd, |guard| {
                let inner: Result<()> = (|| {
                    guard.focus(chat.composer)?;
                    if !self.win.composer_empty(&chat)? {
                        return Err(KaxError::blocked("existing_draft_preserved"));
                    }
                    let (_, _, width, height) = self.win.api.rect(chat.hwnd, true)?;
                    // Hit main send-button body safely left of split dropdown divider (w-33).
                    let x = width - self.win.scale(chat.hwnd, 46);
                    let y = height - self.win.scale(chat.hwnd, 22);
                    let (_, _, _, editor_bottom) = self.win.api.rect(chat.composer, false)?;
                    if self.win.api.point(chat.hwnd, x, y, true)?.1 <= editor_bottom {
                        return Err(KaxError::blocked("send_button_geometry_invalid"));
                    }
                    guard.visible_point(chat.hwnd, x, y)?;
                    written = true;
                    self.win.api.replace_edit(chat.composer, text)?;
                    if composer_text_normalized(&self.win.api.control_text(chat.composer)?)
                        != composer_text_normalized(text)
                    {
                        return Err(KaxError::blocked("composer_text_not_exact"));
                    }
                    if !until_bool(
                        || self.win.send_button_ready(&chat).unwrap_or(false),
                        0.45,
                        0.025,
                    ) {
                        return Err(KaxError::blocked("send_button_not_enabled"));
                    }
                    self.bound_chat(record)?;
                    if composer_text_normalized(&self.win.api.control_text(chat.composer)?)
                        != composer_text_normalized(text)
                    {
                        return Err(KaxError::blocked("composer_text_not_exact"));
                    }
                    guard.check()?;
                    attempted = true;
                    guard.click(chat.hwnd, x, y)?;
                    Ok(())
                })();

                if written && !attempted {
                    if guard.check().is_ok() {
                        if let Ok(cur) = self.bound_chat(record) {
                            if cur.identity() == chat.identity() {
                                if let Ok(cur_txt) = self.win.api.control_text(chat.composer) {
                                    if composer_text_normalized(&cur_txt)
                                        == composer_text_normalized(text)
                                    {
                                        let _ = self.win.api.replace_edit(chat.composer, "");
                                    }
                                }
                            }
                        }
                    }
                }
                inner
            })?;
            let mut m = Map::new();
            m.insert("dispatchAttempted".to_string(), json!(true));
            Ok(response("dispatch_requested", WIN32_SOURCE, m))
        })();

        match res {
            Ok(v) => Ok(v),
            Err(KaxError::Blocked(r)) => {
                let mut m = Map::new();
                m.insert("reason".to_string(), json!(r));
                m.insert("dispatchAttempted".to_string(), json!(attempted));
                Ok(response(
                    if attempted {
                        "dispatch_result_unknown"
                    } else {
                        "blocked"
                    },
                    WIN32_SOURCE,
                    m,
                ))
            }
            Err(e) => Err(e),
        }
    }

    fn read_for_confirmation(&self, record: &Map<String, Value>) -> Result<Value> {
        let res: Result<Value> = (|| {
            let chat = self.bound_chat(record)?;
            let result = self.win.read_messages(&chat.name, 500, 0, "")?;
            self.win.check_chat(&chat, true)?;
            Ok(result)
        })();
        match res {
            Ok(v) => Ok(v),
            Err(KaxError::Blocked(r)) => Ok(blocked_response(CLIPBOARD_SOURCE, &r)),
            Err(e) => Err(e),
        }
    }

    fn inspect_for_confirmation(&self, record: &Map<String, Value>) -> Result<Value> {
        let chat = self.bound_chat(record)?;
        let st = if self.win.composer_empty(&chat)? {
            "ready"
        } else {
            "blocked"
        };
        Ok(response(st, WIN32_SOURCE, Map::new()))
    }
}

impl<A: Win32Api, C: ClipboardBackend<A>, O: OcrBackend> KakaoBackend for WindowsAx<A, C, O> {
    fn build(&self, kind: &str) -> Result<Value> {
        if !matches!(kind, "send" | "read" | "control") {
            return Err(KaxError::validation("Unknown native bridge kind"));
        }
        let started = Instant::now();
        let _dpi = self.api.dpi_aware();
        match (|| -> Result<()> {
            self.app(false)?;
            self.ocr.check_available("ko")?;
            Ok(())
        })() {
            Ok(()) => {
                let elapsed = (started.elapsed().as_secs_f64() * 1000.0 * 10.0).round() / 10.0;
                Ok(json!({
                    "status": "built",
                    "kind": kind,
                    "elapsedMs": elapsed,
                }))
            }
            Err(KaxError::Blocked(r)) => Err(KaxError::validation(r)),
            Err(e) => Err(e),
        }
    }

    fn windows(&self) -> Result<Value> {
        let _dpi = self.api.dpi_aware();
        let res: Result<Value> = (|| {
            let app = self.app(false)?;
            let chats = self.chats(&app)?;
            let mut counts: HashMap<&str, usize> = HashMap::new();
            for c in &chats {
                *counts.entry(c.name.as_str()).or_insert(0) += 1;
            }
            let reason = match self.check_ready(&app) {
                Ok(()) => None,
                Err(KaxError::Blocked(r)) => Some(r),
                Err(e) => return Err(e),
            };
            let items: Vec<Value> = chats
                .iter()
                .map(|chat| {
                    let readable = reason.is_none()
                        && counts.get(chat.name.as_str()).copied().unwrap_or(0) == 1
                        && self.api.is_window_enabled(chat.hwnd);
                    let mut obj = Map::new();
                    obj.insert("windowTitle".to_string(), json!(chat.name));
                    obj.insert("minimized".to_string(), json!(self.api.is_iconic(chat.hwnd)));
                    obj.insert("readable".to_string(), json!(readable));
                    if let Some(ref r) = reason {
                        obj.insert("reason".to_string(), json!(r));
                    }
                    Value::Object(obj)
                })
                .collect();
            let mut m = Map::new();
            m.insert("processId".to_string(), json!(app.pid));
            m.insert("items".to_string(), Value::Array(items));
            Ok(response("ok", WIN32_SOURCE, m))
        })();
        match res {
            Ok(v) => Ok(v),
            Err(KaxError::Blocked(r)) => {
                let mut m = Map::new();
                m.insert("reason".to_string(), json!(r));
                m.insert("items".to_string(), json!([]));
                Ok(response("blocked", WIN32_SOURCE, m))
            }
            Err(e) => Err(e),
        }
    }

    fn rooms(&self, query: &str, scroll: bool, limit: i64) -> Result<Value> {
        validate_win_text(validate_query_text(query, true)?)?;
        validate_integer(limit, "limit", 1, MAX_ROOM_SCROLL_LIMIT)?;
        if scroll {
            // Same structured shape as every other Windows block, so MCP callers get the reason.
            return Ok(blocked_response(OCR_SOURCE, "room_list_scroll_unsupported"));
        }
        self.run_desktop(OCR_SOURCE, || {
            let app = self.app(true)?;
            let controls = self.room_controls(&app)?;
            let previous = self.api.control_text(controls.0)?;
            let listing = if previous.is_empty() {
                controls.1
            } else {
                controls.2
            };
            if !self.api.is_window_visible(listing) {
                return Err(KaxError::blocked("room_list_not_visible"));
            }
            let items = self.list_rows(listing)?;
            let needle = caseless::default_case_fold_str(query);
            let filtered: Vec<RoomItem> = items
                .iter()
                .filter(|i| caseless::default_case_fold_str(&i.name).contains(&needle))
                .cloned()
                .collect();
            Ok(Self::room_output(&filtered, query, &previous, items.len()))
        })
    }

    fn search_rooms(&self, query: &str) -> Result<Value> {
        validate_win_text(validate_query_text(query, true)?)?;
        self.run_desktop(OCR_SOURCE, || {
            let app = self.app(true)?;
            self.api.idle(400)?;
            let controls = self.room_controls(&app)?;
            let listing = self.room_query(controls, query)?;
            let items = self.list_rows(listing)?;
            Ok(Self::room_output(&items, query, query, items.len()))
        })
    }

    fn open_room(&self, window_title: &str, require_self: bool) -> Result<Value> {
        validate_win_text(validate_title(window_title)?)?;
        self.run_desktop(WIN32_SOURCE, || {
            let app = self.app(true)?;
            let existing = self.exact_chat(&app, window_title, true)?;
            if require_self {
                let target_name = if existing.is_none() && is_self_alias(window_title) {
                    None
                } else {
                    Some(window_title)
                };
                let (chat, already) = self.verify_self(&app, target_name, existing.as_ref())?;
                let mut m = Map::new();
                m.insert("windowTitle".to_string(), json!(chat.name));
                m.insert("alreadyOpen".to_string(), json!(already));
                m.insert("isSelf".to_string(), json!(true));
                return Ok(response("ok", WIN32_SOURCE, m));
            }
            if let Some(chat) = existing {
                let mut m = Map::new();
                m.insert("windowTitle".to_string(), json!(chat.name));
                m.insert("alreadyOpen".to_string(), json!(true));
                return Ok(response("ok", WIN32_SOURCE, m));
            }

            let original_fg = self.api.get_foreground_window();
            let mut allowed_ticks = HashSet::new();
            allowed_ticks.insert(self.api.idle(400)?);

            let check_no_input = |ticks: &HashSet<u32>| -> Result<()> {
                if !ticks.contains(&self.api.last_input()?) {
                    return Err(KaxError::blocked("user_input_active"));
                }
                Ok(())
            };

            let controls = self.room_controls(&app)?;
            let previous = self.api.control_text(controls.0)?;
            let outcome: Result<Value> = (|| {
                check_no_input(&allowed_ticks)?;
                let listing = self.room_query(controls, window_title)?;
                let rows = self.list_rows(listing)?;
                let key_target = compact_ws(window_title);
                let matches: Vec<&RoomItem> = rows
                    .iter()
                    .filter(|r| compact_ws(&r.name) == key_target)
                    .collect();
                if matches.len() != 1 {
                    return Err(KaxError::blocked(if matches.is_empty() {
                        "room_not_in_exposed_list"
                    } else {
                        "target_room_ambiguous"
                    }));
                }
                let before: HashSet<usize> = self.chats(&app)?.into_iter().map(|c| c.hwnd).collect();
                check_no_input(&allowed_ticks)?;
                self.check_modal(&app)?;
                let verified = self.list_rows(listing)?;
                let verified_matches: Vec<&RoomItem> = verified
                    .iter()
                    .filter(|r| compact_ws(&r.name) == key_target)
                    .collect();
                if verified_matches.len() != 1 || verified_matches[0].point != matches[0].point {
                    return Err(KaxError::blocked("room_list_changed"));
                }
                check_no_input(&allowed_ticks)?;
                let (px, py) = matches[0].point;
                self.api.click(listing, px, py, true, false)?;
                let new_chats = until(
                    || {
                        let list: Vec<ChatInfo> = self
                            .chats(&app)
                            .ok()?
                            .into_iter()
                            .filter(|c| !before.contains(&c.hwnd))
                            .collect();
                        if list.is_empty() {
                            None
                        } else {
                            Some(list)
                        }
                    },
                    1.5,
                    0.025,
                );
                let Some(new_chats) = new_chats else {
                    return Err(KaxError::blocked("room_open_not_observed"));
                };
                if new_chats.len() != 1 {
                    return Err(KaxError::blocked("room_open_not_observed"));
                }
                let opened = new_chats.into_iter().next().unwrap();
                if opened.name != window_title {
                    let _ = self.close_chat(&opened);
                    return Err(KaxError::blocked("target_room_title_mismatch"));
                }
                self.check_chat(&opened, true)?;
                let mut m = Map::new();
                m.insert("windowTitle".to_string(), json!(opened.name));
                m.insert("alreadyOpen".to_string(), json!(false));
                m.insert("roomSearchUsed".to_string(), json!(true));
                m.insert("roomSearchRestored".to_string(), json!(true));
                Ok(response("ok", WIN32_SOURCE, m))
            })();

            if self.api.control_text(controls.0).ok().as_deref() == Some(window_title) {
                let _ = self.room_query(controls, &previous);
            }
            self.restore_room_foreground(&app, original_fg, &mut allowed_ticks)?;
            outcome
        })
    }

    fn members(&self, window_title: &str, limit: i64) -> Result<Value> {
        validate_win_text(validate_title(window_title)?)?;
        validate_integer(limit, "limit", 1, MAX_MEMBER_LIMIT)?;
        // Structured like every other Windows block, so MCP callers get the reason.
        Ok(blocked_response(WIN32_SOURCE, "member_list_unsupported"))
    }

    fn close_room(&self, window_title: &str) -> Result<Value> {
        validate_win_text(validate_title(window_title)?)?;
        self.run_desktop(WIN32_SOURCE, || {
            let chat = self.exact_chat(&self.app(true)?, window_title, true)?;
            let already_closed = chat.is_none();
            if let Some(c) = chat {
                self.close_chat(&c)?;
            }
            let mut m = Map::new();
            m.insert("windowTitle".to_string(), json!(window_title));
            m.insert("alreadyClosed".to_string(), json!(already_closed));
            Ok(response("ok", WIN32_SOURCE, m))
        })
    }

    fn read_messages(
        &self,
        window_title: &str,
        limit: i64,
        offset: i64,
        snapshot_id: &str,
    ) -> Result<Value> {
        validate_win_text(validate_title(window_title)?)?;
        validate_integer(limit, "limit", 1, 500)?;
        validate_integer(offset, "offset", 0, 1000)?;
        validate_snapshot_id_win32(snapshot_id, offset > 0)?;

        self.run_desktop(CLIPBOARD_SOURCE, || {
            let chat = self.exact_chat(&self.app(true)?, window_title, false)?.unwrap();
            let (actual, all_rows, elapsed) = self.snapshot(&chat, None)?;
            if !snapshot_id.is_empty() && snapshot_id != actual {
                let mut m = Map::new();
                m.insert(
                    "reason".to_string(),
                    json!("Clipboard rows changed; restart at offset 0"),
                );
                m.insert("snapshotId".to_string(), json!(actual));
                m.insert("items".to_string(), json!([]));
                m.insert("uiActions".to_string(), json!(1));
                return Ok(response("snapshot_changed", CLIPBOARD_SOURCE, m));
            }
            let exposed = all_rows.len();
            let row_start = exposed.saturating_sub(1000);
            let rows = &all_rows[row_start..];
            let end = rows.len().saturating_sub(offset as usize);
            let start = end.saturating_sub(limit as usize);

            let text_count = rows
                .iter()
                .filter(|r| r.get("kind").and_then(|v| v.as_str()) == Some("text"))
                .count();
            let unclassified_count = rows
                .iter()
                .filter(|r| r.get("kind").and_then(|v| v.as_str()) == Some("unclassified"))
                .count();
            let attachment_count = rows
                .iter()
                .filter(|r| r.get("kind").and_then(|v| v.as_str()) == Some("attachment"))
                .count();

            let next_offset = if start > 0 {
                json!(offset as usize + (end - start))
            } else {
                Value::Null
            };

            let mut m = Map::new();
            m.insert("windowTitle".to_string(), json!(chat.name));
            m.insert("items".to_string(), Value::Array(rows[start..end].to_vec()));
            m.insert("total".to_string(), json!(rows.len()));
            m.insert("limit".to_string(), json!(limit));
            m.insert("offset".to_string(), json!(offset));
            m.insert("hasMore".to_string(), json!(start > 0));
            m.insert("nextOffset".to_string(), next_offset);
            m.insert("snapshotId".to_string(), json!(actual));
            m.insert("textExact".to_string(), json!(true));
            m.insert("textMessageCount".to_string(), json!(text_count));
            m.insert(
                "unclassifiedRowCount".to_string(),
                json!(unclassified_count),
            );
            m.insert("attachmentRowCount".to_string(), json!(attachment_count));
            m.insert(
                "coverage".to_string(),
                json!({
                    "scope": "currently_copied_chat_rows",
                    "historyComplete": false,
                    "consistency": "best_effort",
                    "exposedRowCount": exposed,
                    "rowsTruncated": exposed > rows.len(),
                    "rowStart": row_start,
                    "rowLimit": 1000,
                    "scrollPerformed": false,
                }),
            );
            m.insert("uiActions".to_string(), json!(1));
            m.insert("elapsedMs".to_string(), json!(elapsed));
            Ok(response("ok", CLIPBOARD_SOURCE, m))
        })
    }

    fn read_many(
        &self,
        window_titles: &[String],
        limit: i64,
        open_if_needed: bool,
        close_after: bool,
        max_workers: i64,
    ) -> Result<Value> {
        if window_titles.is_empty() || window_titles.len() > 20 {
            return Err(KaxError::validation(
                "window_titles must contain 1..20 exact titles",
            ));
        }
        let mut seen = HashSet::new();
        for name in window_titles {
            validate_win_text(validate_title(name)?)?;
            if !seen.insert(name.as_str()) {
                return Err(KaxError::validation(
                    "Duplicate window_titles are not allowed",
                ));
            }
        }
        validate_integer(limit, "limit", 1, 500)?;
        validate_integer(max_workers, "max_workers", 1, 4)?;

        self.run_desktop(CLIPBOARD_SOURCE, || {
            let started = Instant::now();
            let mut items = Vec::new();
            let mut opened: Vec<ChatInfo> = Vec::new();
            let mut cleanup = Vec::new();

            for name in window_titles {
                let begin = Instant::now();
                let res = (|| -> Result<Value> {
                    let open_res = if open_if_needed {
                        self.open_room(name, false)?
                    } else {
                        json!({"status": "ok", "alreadyOpen": true})
                    };
                    if open_res.get("status").and_then(|v| v.as_str()) != Some("ok") {
                        return Ok(open_res);
                    }
                    if open_res.get("alreadyOpen").and_then(|v| v.as_bool()) != Some(true) {
                        if let Some(c) = self.exact_chat(&self.app(false)?, name, false)? {
                            opened.push(c);
                        }
                    }
                    self.read_messages(name, limit, 0, "")
                })();

                let val = match res {
                    Ok(v) => v,
                    Err(KaxError::Blocked(r)) => blocked_response(CLIPBOARD_SOURCE, &r),
                    Err(e) => {
                        let mut m = Map::new();
                        m.insert("errorType".to_string(), json!(e.error_type()));
                        response("failed", CLIPBOARD_SOURCE, m)
                    }
                };
                let mut obj = val.as_object().cloned().unwrap_or_default();
                obj.insert("windowTitle".to_string(), json!(name));
                obj.insert(
                    "callElapsedMs".to_string(),
                    json!((begin.elapsed().as_secs_f64() * 1000.0 * 10.0).round() / 10.0),
                );
                items.push(Value::Object(obj));
            }

            if close_after {
                for chat in opened.iter().rev() {
                    match self.close_chat(chat) {
                        Ok(()) => {
                            let mut m = Map::new();
                            m.insert("windowTitle".to_string(), json!(chat.name));
                            m.insert("alreadyClosed".to_string(), json!(false));
                            cleanup.push(response("ok", WIN32_SOURCE, m));
                        }
                        Err(KaxError::Blocked(r)) => {
                            let mut m = Map::new();
                            m.insert("reason".to_string(), json!(r));
                            m.insert("windowTitle".to_string(), json!(chat.name));
                            cleanup.push(response("blocked", WIN32_SOURCE, m));
                        }
                        Err(e) => {
                            let mut m = Map::new();
                            m.insert("errorType".to_string(), json!(e.error_type()));
                            m.insert("windowTitle".to_string(), json!(chat.name));
                            cleanup.push(response("failed", WIN32_SOURCE, m));
                        }
                    }
                }
            }

            let succeeded = items
                .iter()
                .filter(|i| i.get("status").and_then(|v| v.as_str()) == Some("ok"))
                .count();
            let cleanup_ok = cleanup
                .iter()
                .all(|c| c.get("status").and_then(|v| v.as_str()) == Some("ok"));
            let status = if succeeded == items.len() && cleanup_ok {
                "ok"
            } else {
                "partial"
            };
            let mut m = Map::new();
            m.insert("items".to_string(), Value::Array(items));
            m.insert("total".to_string(), json!(window_titles.len()));
            m.insert("succeeded".to_string(), json!(succeeded));
            m.insert(
                "windowsOpened".to_string(),
                json!(opened.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()),
            );
            m.insert("cleanup".to_string(), Value::Array(cleanup));
            m.insert("parallel".to_string(), json!(false));
            m.insert("atomicAcrossRooms".to_string(), json!(false));
            m.insert(
                "elapsedMs".to_string(),
                json!((started.elapsed().as_secs_f64() * 1000.0 * 10.0).round() / 10.0),
            );
            Ok(response(status, CLIPBOARD_SOURCE, m))
        })
    }

    fn load_more(&self, window_title: &str, pages: i64) -> Result<Value> {
        validate_win_text(validate_title(window_title)?)?;
        validate_integer(pages, "pages", 1, 10)?;
        self.run_desktop(CLIPBOARD_SOURCE, || {
            load_more_impl(
                CLIPBOARD_SOURCE,
                window_title,
                pages,
                LoadProbe::FullRead,
                || self.read_messages(window_title, 500, 0, ""),
                || self.control_step("load_older", window_title, ""),
            )
        })
    }

    fn search_messages(
        &self,
        window_title: &str,
        query: &str,
        limit: i64,
        load_pages: i64,
        case_sensitive: bool,
        use_app_search: bool,
        max_steps: i64,
        context_rows: i64,
    ) -> Result<Value> {
        validate_win_text(validate_title(window_title)?)?;
        validate_win_text(validate_query_text(query, false)?)?;
        validate_integer(limit, "limit", 1, 500)?;
        validate_integer(load_pages, "load_pages", 0, 10)?;
        validate_integer(max_steps, "max_steps", 1, 50)?;
        validate_integer(context_rows, "context_rows", 0, 10)?;

        self.run_desktop(CLIPBOARD_SOURCE, || {
            let mut result = search_messages_impl(
                CLIPBOARD_SOURCE,
                window_title,
                query,
                limit as usize,
                load_pages,
                case_sensitive,
                use_app_search,
                max_steps as usize,
                context_rows as usize,
                |row| row.get("kind").and_then(|v| v.as_str()) == Some("text"),
                || self.open_room(window_title, false),
                |p| self.load_more(window_title, p),
                |offset, snap| self.read_messages(window_title, 500, offset, snap),
                |action| self.control_step(action, window_title, query),
            )?;
            let items_empty = result
                .get("items")
                .and_then(|v| v.as_array())
                .map(|a| a.is_empty())
                .unwrap_or(true);
            if result.get("stopReason").and_then(|v| v.as_str()) == Some("user_input_active")
                && items_empty
            {
                if let Some(obj) = result.as_object_mut() {
                    obj.insert("status".to_string(), json!("blocked"));
                    obj.insert("reason".to_string(), json!("user_input_active"));
                }
            } else if use_app_search
                && result.get("searchVerified") == Some(&Value::Bool(true))
                && items_empty
            {
                if let Some(obj) = result.as_object_mut() {
                    obj.insert("status".to_string(), json!("partial"));
                    obj.insert("searchVerified".to_string(), json!(false));
                    obj.insert("stopReason".to_string(), json!("search_result_not_observed"));
                }
            }
            Ok(result)
        })
    }

    fn close_search(&self, window_title: &str) -> Result<Value> {
        validate_win_text(validate_title(window_title)?)?;
        self.run_desktop(WIN32_SOURCE, || {
            let chat = self.exact_chat(&self.app(true)?, window_title, false)?.unwrap();
            if !self.search_active(&chat) {
                let mut m = Map::new();
                m.insert("windowTitle".to_string(), json!(chat.name));
                m.insert("alreadyClosed".to_string(), json!(true));
                return Ok(response("ok", WIN32_SOURCE, m));
            }
            let search = chat.search.unwrap();
            ForegroundGuard::with_activated(self.api.as_ref(), chat.hwnd, |guard| {
                let draft = self.api.control_text(chat.composer)?;
                let (_, top, _, bottom) = self.api.rect(search, false)?;
                let (_, y) = self.api.point(chat.hwnd, 0, (top + bottom) / 2, false)?;
                let (_, _, width, _) = self.api.rect(chat.hwnd, true)?;
                guard.click(chat.hwnd, width - self.scale(chat.hwnd, 23), y)?;
                if !until_bool(|| !self.search_active(&chat), 0.7, 0.025) {
                    return Err(KaxError::blocked("search_close_not_observed"));
                }
                if !draft.is_empty()
                    && !PLACEHOLDERS.contains(&draft.as_str())
                    && self.api.control_text(chat.composer)? != draft
                {
                    return Err(KaxError::blocked("composer_changed"));
                }
                Ok(())
            })?;
            let mut m = Map::new();
            m.insert("windowTitle".to_string(), json!(chat.name));
            m.insert("alreadyClosed".to_string(), json!(false));
            Ok(response("ok", WIN32_SOURCE, m))
        })
    }

    fn read_attachment(
        &self,
        window_title: &str,
        row_index: i64,
        snapshot_id: &str,
        output_path: Option<&str>,
        ocr: bool,
    ) -> Result<Value> {
        validate_win_text(validate_title(window_title)?)?;
        validate_integer(row_index, "row_index", 0, 1_000_000)?;
        validate_snapshot_id_win32(snapshot_id, true)?;
        validate_output_path(output_path)?;

        self.run_desktop(WIN32_SOURCE, || {
            let chat = self.exact_chat(&self.app(true)?, window_title, false)?.unwrap();
            enum LocateOutcome {
                Found(crate::win32_image::Frame, bool),
                SnapshotChanged(Value),
            }
            let (image, restored) = match ForegroundGuard::with_activated(
                self.api.as_ref(),
                chat.hwnd,
                |guard| {
                    let (current, rows, _) = self.snapshot(&chat, Some(guard))?;
                    if current != snapshot_id {
                        let mut m = Map::new();
                        m.insert("reason".to_string(), json!("Read the attachment row again"));
                        m.insert("snapshotId".to_string(), json!(current));
                        return Ok(LocateOutcome::SnapshotChanged(response(
                            "snapshot_changed",
                            WIN32_SOURCE,
                            m,
                        )));
                    }
                    let row = rows.get(row_index as usize);
                    let has_image = row
                        .and_then(|r| r.get("attachments"))
                        .and_then(|a| a.as_array())
                        .map(|arr| {
                            arr.iter().any(|item| {
                                item.get("type").and_then(|t| t.as_str()) == Some("image")
                            })
                        })
                        .unwrap_or(false);
                    if !has_image {
                        return Err(KaxError::blocked("attachment_row_required"));
                    }
                    let host = WinPhotoHost {
                        win: self,
                        chat: &chat,
                    };
                    let (img, rest) =
                        PhotoLocator::new(&host, guard, &rows, row_index as usize).locate()?;
                    let (current_after, _, _) = self.snapshot(&chat, Some(guard))?;
                    if current_after != snapshot_id {
                        let mut m = Map::new();
                        m.insert("reason".to_string(), json!("Read the attachment row again"));
                        m.insert("snapshotId".to_string(), json!(current_after));
                        return Ok(LocateOutcome::SnapshotChanged(response(
                            "snapshot_changed",
                            WIN32_SOURCE,
                            m,
                        )));
                    }
                    Ok(LocateOutcome::Found(img, rest))
                },
            )? {
                LocateOutcome::Found(img, rest) => (img, rest),
                LocateOutcome::SnapshotChanged(val) => return Ok(val),
            };

            let mut m = Map::new();
            if ocr {
                let lines = self.ocr.recognize("ko", &image)?;
                let joined = lines
                    .into_iter()
                    .map(|l| l.text)
                    .collect::<Vec<_>>()
                    .join("\n");
                m.insert("ocr".to_string(), json!(joined));
                m.insert("ocrSource".to_string(), json!(OCR_SOURCE));
            }
            let png = image.png()?;
            let final_path = write_private_png(&png, output_path)?;
            m.insert("windowTitle".to_string(), json!(chat.name));
            m.insert("rowIndex".to_string(), json!(row_index));
            m.insert("snapshotId".to_string(), json!(snapshot_id));
            m.insert(
                "imagePath".to_string(),
                json!(final_path.display().to_string()),
            );
            m.insert("width".to_string(), json!(image.width));
            m.insert("height".to_string(), json!(image.height));
            m.insert("scrollRestored".to_string(), json!(restored));
            m.insert("ocrPerformed".to_string(), json!(ocr));
            Ok(response("ok", WIN32_SOURCE, m))
        })
    }

    fn send_message(
        &self,
        window_title: &str,
        text: &str,
        operation_id: &str,
        wait_seconds: f64,
        require_self: bool,
    ) -> Result<Value> {
        validate_win_text(validate_title(window_title)?)?;
        validate_win_text(text)?;
        self.run_desktop(WIN32_SOURCE, || {
            let adapter = WindowsSendAdapter { win: self };
            SendEngine::new(&adapter).send(
                window_title,
                text,
                operation_id,
                wait_seconds,
                require_self,
            )
        })
    }

    fn send_result(&self, operation_id: &str, wait_seconds: f64) -> Result<Value> {
        self.run_desktop(WIN32_SOURCE, || {
            let adapter = WindowsSendAdapter { win: self };
            SendEngine::new(&adapter).result(operation_id, wait_seconds)
        })
    }
}
