//! Platform-agnostic Win32 abstraction trait, constants, and polling helpers.

use std::time::{Duration, Instant};

use crate::error::Result;
use crate::win32_image::Frame;
use crate::win32_rows::OcrLine;

pub const WM_SETTEXT: u32 = 0x000C;
pub const WM_GETTEXT: u32 = 0x000D;
pub const WM_GETTEXTLENGTH: u32 = 0x000E;
pub const WM_CLOSE: u32 = 0x0010;
pub const WM_NULL: u32 = 0x0000;
pub const WM_KEYDOWN: u32 = 0x0100;
pub const WM_KEYUP: u32 = 0x0101;
pub const WM_MOUSEMOVE: u32 = 0x0200;
pub const WM_LBUTTONDOWN: u32 = 0x0201;
pub const WM_LBUTTONUP: u32 = 0x0202;
pub const WM_LBUTTONDBLCLK: u32 = 0x0203;
pub const WM_RBUTTONDOWN: u32 = 0x0204;
pub const WM_RBUTTONUP: u32 = 0x0205;
pub const WM_MOUSEWHEEL: u32 = 0x020A;
pub const EM_SETSEL: u32 = 0x00B1;
pub const EM_REPLACESEL: u32 = 0x00C2;

pub const VK_CONTROL: u16 = 0x11;
pub const VK_RETURN: u16 = 0x0D;
pub const VK_ESCAPE: u16 = 0x1B;
pub const VK_HOME: u16 = 0x24;
pub const VK_PRIOR: u16 = 0x21;
pub const VK_END: u16 = 0x23;
pub const VK_UNASSIGNED: u16 = 0xE8;

pub const PW_RENDERFULLCONTENT: u32 = 2;
pub const CF_UNICODETEXT: u32 = 13;

pub const MODIFIER_KEYS: &[i32] = &[1, 2, 4, 0x10, 0x11, 0x12, 0x5B, 0x5C];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub hwnd: usize,
    pub cls: String,
    pub name: String,
    pub control_id: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    Key { vk: u16, flags: u32, timestamp: u32 },
    Mouse { x: i32, y: i32, flags: u32, timestamp: u32 },
}

pub fn packed(x: i32, y: i32) -> isize {
    ((x as u32 & 0xFFFF) | (((y as u32) & 0xFFFF) << 16)) as isize
}

/// Compute the background gutter click coordinate for `arm_list`, or `None` if too short.
pub fn arm_list_click_target(client_height: i32) -> Option<(i32, i32)> {
    if client_height < 16 {
        None
    } else {
        Some((4, client_height - 8))
    }
}

/// Shared production `arm_list` helper: posts a background gutter click only when tall enough.
pub fn arm_list_with_ops<F>(hwnd: usize, client_height: i32, mut click_fn: F) -> Result<()>
where
    F: FnMut(usize, i32, i32) -> Result<()>,
{
    if let Some((x, y)) = arm_list_click_target(client_height) {
        click_fn(hwnd, x, y)?;
    }
    Ok(())
}

/// Check whether `last_input` qualifies as idle given current `tick`, `milliseconds` threshold,
/// previously injected `own_ticks`, and whether any modifier/mouse key is currently held.
pub fn is_idle_tick(
    tick: u32,
    last_input: u32,
    milliseconds: u32,
    own_ticks: &[u32],
    any_modifier_down: bool,
) -> bool {
    if any_modifier_down {
        return false;
    }
    let elapsed = tick.wrapping_sub(last_input);
    (elapsed >= milliseconds && elapsed < 0x8000_0000) || own_ticks.contains(&last_input)
}

/// Shared production `send_input` helper: dispatches events via `dispatch_fn` and records
/// each event's timestamp into `own_ticks` only after `SendInput` succeeds.
pub fn send_input_with_ops<F>(
    events: &[InputEvent],
    own_ticks: &std::sync::Mutex<std::collections::VecDeque<u32>>,
    mut dispatch_fn: F,
) -> Result<()>
where
    F: FnMut(&[InputEvent]) -> Result<()>,
{
    dispatch_fn(events)?;
    let mut ticks = own_ticks.lock().unwrap();
    for &ev in events {
        let stamp = match ev {
            InputEvent::Key { timestamp, .. } | InputEvent::Mouse { timestamp, .. } => timestamp,
        };
        if stamp != 0 && !ticks.contains(&stamp) {
            if ticks.len() >= 32 {
                ticks.pop_front();
            }
            ticks.push_back(stamp);
        }
    }
    Ok(())
}

/// Low-level thread-attach / foreground unlock primitives (used by `Win32Live` and unit tests).
pub trait ForegroundUnlockOps {
    fn is_window(&self, hwnd: usize) -> bool;
    fn is_iconic(&self, hwnd: usize) -> bool;
    fn get_foreground_window(&self) -> usize;
    fn pump_messages(&self);
    fn current_thread_id(&self) -> u32;
    fn window_thread_id(&self, hwnd: usize) -> u32;
    fn attach_thread_input(&self, current: u32, target: u32, attach: bool) -> bool;
    fn bring_window_to_top(&self, hwnd: usize);
    fn set_foreground_window(&self, hwnd: usize);
    fn any_modifier_down(&self) -> bool;
    fn last_input(&self) -> Result<u32>;
    fn tick(&self) -> u32;
    fn send_unlock_key(&self, stamp: u32) -> Result<()>;
}

/// Shared production foreground-activation algorithm (thread-input attach with `VK_UNASSIGNED` fallback).
pub fn activate_foreground_with_ops<O: ForegroundUnlockOps>(
    ops: &O,
    hwnd: usize,
    mut allowed_ticks: Option<&mut std::collections::HashSet<u32>>,
    seconds: f64,
) -> bool {
    if !ops.is_window(hwnd) {
        return false;
    }
    if ops.get_foreground_window() == hwnd && !ops.is_iconic(hwnd) {
        return true;
    }
    let try_attach = || -> bool {
        ops.pump_messages();
        if ops.is_iconic(hwnd) {
            return false;
        }
        let fg = ops.get_foreground_window();
        if fg == hwnd {
            return true;
        }
        let current = ops.current_thread_id();
        let fg_tid = if fg != 0 { ops.window_thread_id(fg) } else { 0 };
        let tgt_tid = ops.window_thread_id(hwnd);
        let attached_fg =
            fg_tid != 0 && fg_tid != current && ops.attach_thread_input(current, fg_tid, true);
        let attached_tgt = tgt_tid != 0
            && tgt_tid != current
            && tgt_tid != fg_tid
            && ops.attach_thread_input(current, tgt_tid, true);
        ops.bring_window_to_top(hwnd);
        ops.set_foreground_window(hwnd);
        if attached_tgt {
            ops.attach_thread_input(current, tgt_tid, false);
        }
        if attached_fg {
            ops.attach_thread_input(current, fg_tid, false);
        }
        ops.get_foreground_window() == hwnd
    };

    if until_bool(try_attach, seconds.min(0.35), 0.025) {
        return true;
    }
    if ops.any_modifier_down() {
        return false;
    }
    if let Some(ref ticks) = allowed_ticks {
        let Ok(last) = ops.last_input() else {
            return false;
        };
        if !ticks.contains(&last) {
            return false;
        }
    }
    let stamp = ops.tick();
    if let Some(ref mut ticks) = allowed_ticks {
        ticks.insert(stamp);
    }
    if ops.send_unlock_key(stamp).is_err() {
        return false;
    }
    until_bool(try_attach, (seconds - 0.35).max(0.15), 0.025)
}

pub fn until<T, F>(mut predicate: F, seconds: f64, interval_secs: f64) -> Option<T>
where
    F: FnMut() -> Option<T>,
{
    let deadline = Instant::now() + Duration::from_secs_f64(seconds.max(0.0));
    let interval = Duration::from_secs_f64(interval_secs.max(0.001));
    loop {
        if let Some(val) = predicate() {
            return Some(val);
        }
        let now = Instant::now();
        if now >= deadline {
            return None;
        }
        std::thread::sleep(interval.min(deadline.saturating_duration_since(now)));
    }
}

pub fn until_bool<F>(mut predicate: F, seconds: f64, interval_secs: f64) -> bool
where
    F: FnMut() -> bool,
{
    until(|| if predicate() { Some(()) } else { None }, seconds, interval_secs).is_some()
}

pub struct DpiGuard {
    restore: Option<Box<dyn FnOnce() + Send>>,
}

impl DpiGuard {
    pub fn noop() -> Self {
        Self { restore: None }
    }

    pub fn new(restore: impl FnOnce() + Send + 'static) -> Self {
        Self {
            restore: Some(Box::new(restore)),
        }
    }
}

impl Drop for DpiGuard {
    fn drop(&mut self) {
        if let Some(f) = self.restore.take() {
            f();
        }
    }
}

/// Abstract Win32 desktop API so all Windows logic can be tested with a mock desktop on any OS.
pub trait Win32Api: Send + Sync {
    fn handles(&self, parent: Option<usize>) -> Vec<usize>;
    fn title(&self, hwnd: usize) -> Result<String>;
    fn class_name(&self, hwnd: usize) -> String;
    fn dlg_ctrl_id(&self, hwnd: usize) -> i32;
    fn window(&self, hwnd: usize) -> Result<Window> {
        Ok(Window {
            hwnd,
            cls: self.class_name(hwnd),
            name: self.title(hwnd)?,
            control_id: self.dlg_ctrl_id(hwnd),
        })
    }
    fn children(&self, hwnd: usize) -> Result<Vec<Window>> {
        let mut out = Vec::new();
        for child in self.handles(Some(hwnd)) {
            out.push(self.window(child)?);
        }
        Ok(out)
    }
    fn pid(&self, hwnd: usize) -> u32;
    fn process(&self, pid: u32) -> (String, u64);

    fn is_window(&self, hwnd: usize) -> bool;
    fn is_window_visible(&self, hwnd: usize) -> bool;
    fn is_window_enabled(&self, hwnd: usize) -> bool;
    fn is_iconic(&self, hwnd: usize) -> bool;
    fn get_parent(&self, hwnd: usize) -> usize;
    fn get_owner(&self, hwnd: usize) -> usize;
    fn get_ancestor_root(&self, hwnd: usize) -> usize;
    fn get_dpi_for_window(&self, hwnd: usize) -> u32;

    fn get_foreground_window(&self) -> usize;
    fn set_foreground_window(&self, hwnd: usize) -> bool;
    fn bring_window_to_top(&self, hwnd: usize) -> bool;
    fn show_window_async(&self, hwnd: usize, cmd: i32) -> bool;
    fn get_async_key_state(&self, vk: i32) -> i16;
    fn window_from_point(&self, x: i32, y: i32) -> usize;

    fn control_text(&self, hwnd: usize) -> Result<String>;
    fn set_text(&self, hwnd: usize, value: &str) -> Result<()>;
    fn replace_edit(&self, hwnd: usize, value: &str) -> Result<()>;
    fn post(&self, hwnd: usize, code: u32, wparam: usize, lparam: isize) -> Result<()>;
    fn click(&self, hwnd: usize, x: i32, y: i32, double: bool, right: bool) -> Result<()>;
    fn wheel(&self, hwnd: usize, delta: i32) -> Result<()>;

    fn tick(&self) -> u32;
    fn last_input(&self) -> Result<u32>;
    fn idle(&self, milliseconds: u32) -> Result<u32>;
    fn focus_hwnd(&self, hwnd: usize) -> Result<usize>;
    fn focus_child(&self, hwnd: usize) -> Result<()>;
    fn activate_foreground(
        &self,
        hwnd: usize,
        allowed_ticks: Option<&mut std::collections::HashSet<u32>>,
        seconds: f64,
    ) -> bool;
    fn pump_messages(&self);

    fn input_keys(&self, keys: &[(u16, bool)], stamp: u32) -> Result<()>;
    fn send_input(&self, events: &[InputEvent]) -> Result<()>;
    fn cursor(&self) -> Result<(i32, i32)>;
    fn mouse_click(&self, x: i32, y: i32, stamp: u32) -> Result<()>;
    fn mouse_move_event(&self, x: i32, y: i32, stamp: u32) -> Result<InputEvent>;
    fn mouse_button_event(&self, flags: u32, stamp: u32) -> InputEvent {
        InputEvent::Mouse {
            x: 0,
            y: 0,
            flags,
            timestamp: stamp,
        }
    }

    fn arm_list(&self, hwnd: usize) -> Result<()>;
    fn clear_selection(&self, hwnd: usize) -> Result<()>;
    fn rect(&self, hwnd: usize, client: bool) -> Result<(i32, i32, i32, i32)>;
    fn point(&self, hwnd: usize, x: i32, y: i32, to_screen: bool) -> Result<(i32, i32)>;
    fn dpi_aware(&self) -> DpiGuard;
    fn capture(&self, hwnd: usize) -> Result<Frame>;
}

/// Abstract OCR engine (live WinRT `Windows.Media.Ocr` on Windows, or mock in tests).
pub trait OcrBackend: Send + Sync {
    fn check_available(&self, language: &str) -> Result<()>;
    fn recognize(&self, language: &str, frame: &Frame) -> Result<Vec<OcrLine>>;
}
