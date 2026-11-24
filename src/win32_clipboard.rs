//! Interactive desktop foreground guard and multi-format Win32 clipboard backup/restore.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use crate::error::{KaxError, Result};
use crate::win32_api::{until, until_bool, Win32Api, CF_UNICODETEXT, MODIFIER_KEYS, VK_CONTROL};

/// RAII guard that activates a target window, guards against concurrent user input,
/// and restores foreground window, child focus, and minimized state on drop.
pub struct ForegroundGuard<'a, A: Win32Api> {
    pub api: &'a A,
    pub allowed_ticks: HashSet<u32>,
    pub initial: usize,
    pub initial_focus: Option<usize>,
    pub target: Option<usize>,
    pub target_focus: Option<usize>,
    pub was_minimized: bool,
    pub own_tick: Option<u32>,
    restored: bool,
}

impl<'a, A: Win32Api> ForegroundGuard<'a, A> {
    pub fn new(api: &'a A) -> Result<Self> {
        let idle_tick = api.idle(400)?;
        let mut allowed_ticks = HashSet::new();
        allowed_ticks.insert(idle_tick);
        let initial = api.get_foreground_window();
        let initial_focus = if initial != 0 {
            api.focus_hwnd(initial).ok().filter(|&h| h != 0)
        } else {
            None
        };
        Ok(Self {
            api,
            allowed_ticks,
            initial,
            initial_focus,
            target: None,
            target_focus: None,
            was_minimized: false,
            own_tick: None,
            restored: false,
        })
    }

    pub fn activate_new(api: &'a A, hwnd: usize) -> Result<Self> {
        let mut guard = Self::new(api)?;
        guard.activate(hwnd)?;
        Ok(guard)
    }

    pub fn check(&self) -> Result<()> {
        let last = self.api.last_input()?;
        if !self.allowed_ticks.contains(&last) {
            return Err(KaxError::blocked("user_input_active"));
        }
        Ok(())
    }

    fn switch(&mut self, hwnd: usize) -> bool {
        if self.api.get_foreground_window() != hwnd {
            self.api.set_foreground_window(hwnd);
        }
        if self.api.get_foreground_window() != hwnd {
            self.api
                .activate_foreground(hwnd, Some(&mut self.allowed_ticks), 0.6);
        }
        until_bool(|| self.api.get_foreground_window() == hwnd, 0.4, 0.025)
    }

    pub fn activate(&mut self, hwnd: usize) -> Result<()> {
        self.check()?;
        if self.api.get_foreground_window() != self.initial {
            return Err(KaxError::blocked("foreground_changed"));
        }
        self.target = Some(hwnd);
        self.was_minimized = self.api.is_iconic(hwnd);
        self.target_focus = self.api.focus_hwnd(hwnd).ok().filter(|&h| h != 0);
        if self.was_minimized {
            self.api.show_window_async(hwnd, 9); // SW_RESTORE
        }
        if !self.switch(hwnd) {
            return Err(KaxError::blocked("foreground_unavailable"));
        }
        if self.was_minimized {
            until_bool(|| !self.api.is_iconic(hwnd), 0.4, 0.025);
        }
        self.api.bring_window_to_top(hwnd);
        if self.target_focus.is_none() {
            self.target_focus = self.api.focus_hwnd(hwnd).ok().filter(|&h| h != 0);
        }
        self.check()
    }

    pub fn focus(&self, child: usize) -> Result<()> {
        self.check()?;
        let target = self.target.unwrap_or(0);
        if self.api.get_foreground_window() != target {
            return Err(KaxError::blocked("foreground_changed"));
        }
        if self.api.focus_hwnd(target)? != child {
            self.api.focus_child(child)?;
        }
        Ok(())
    }

    pub fn input_stamp(&mut self) -> Result<u32> {
        for &vk in MODIFIER_KEYS {
            if (self.api.get_async_key_state(vk) as u16 & 0x8000) != 0 {
                return Err(KaxError::blocked("user_input_active"));
            }
        }
        let last = self.api.last_input()?;
        if !self.allowed_ticks.contains(&last) {
            return Err(KaxError::blocked("user_input_active"));
        }
        let stamp = self.api.tick();
        self.allowed_ticks.insert(stamp);
        self.own_tick = Some(stamp);
        Ok(stamp)
    }

    pub fn keys(&mut self, child: usize, keys: &[u16], control: bool) -> Result<()> {
        self.check()?;
        let target = self.target.unwrap_or(0);
        if self.api.get_foreground_window() != target || self.api.focus_hwnd(target)? != child {
            return Err(KaxError::blocked("foreground_changed"));
        }
        let mut sequence = Vec::new();
        for &key in keys {
            if control {
                sequence.push((VK_CONTROL, false));
            }
            sequence.push((key, false));
            sequence.push((key, true));
            if control {
                sequence.push((VK_CONTROL, true));
            }
        }
        let stamp = self.input_stamp()?;
        self.api.input_keys(&sequence, stamp)
    }

    pub fn visible_point(&self, hwnd: usize, x: i32, y: i32) -> Result<(i32, i32)> {
        self.check()?;
        let target = self.target.unwrap_or(0);
        if self.api.get_foreground_window() != target {
            return Err(KaxError::blocked("foreground_changed"));
        }
        let screen = self.api.point(hwnd, x, y, true)?;
        let hit = self.api.window_from_point(screen.0, screen.1);
        if hit == 0 || self.api.get_ancestor_root(hit) != target || hit != hwnd {
            return Err(KaxError::blocked("click_target_obscured"));
        }
        Ok(screen)
    }

    fn restore_cursor(&self, position: (i32, i32), at: (i32, i32)) {
        let Ok(last) = self.api.last_input() else {
            return;
        };
        if !self.allowed_ticks.contains(&last) {
            return;
        }
        if let Ok(cur) = self.api.cursor() {
            if cur == at && position != at {
                if let Some(stamp) = self.own_tick {
                    if let Ok(ev) = self.api.mouse_move_event(position.0, position.1, stamp) {
                        let _ = self.api.send_input(&[ev]);
                    }
                }
            }
        }
    }

    pub fn click(&mut self, hwnd: usize, x: i32, y: i32) -> Result<()> {
        let screen = self.visible_point(hwnd, x, y)?;
        let position = self.api.cursor()?;
        let stamp = self.input_stamp()?;
        let res = (|| -> Result<()> {
            self.api.mouse_click(screen.0, screen.1, stamp)?;
            let _ = until_bool(
                || self.api.last_input().ok() == self.own_tick,
                0.15,
                0.015,
            );
            std::thread::sleep(Duration::from_millis(120));
            Ok(())
        })();
        self.restore_cursor(position, screen);
        res
    }

    pub fn drag(
        &mut self,
        hwnd: usize,
        start: (i32, i32),
        end: (i32, i32),
        via: &[(i32, i32)],
    ) -> Result<()> {
        let origin = self.visible_point(hwnd, start.0, start.1)?;
        let mut destinations = Vec::with_capacity(via.len() + 1);
        for &(vx, vy) in via {
            destinations.push(self.visible_point(hwnd, vx, vy)?);
        }
        destinations.push(self.visible_point(hwnd, end.0, end.1)?);
        let cursor = self.api.cursor()?;
        let stamp = self.input_stamp()?;
        let mut moved = origin;
        let target = self.target.unwrap_or(0);

        let res = (|| -> Result<()> {
            let mv = self.api.mouse_move_event(origin.0, origin.1, stamp)?;
            let dn = self.api.mouse_button_event(2, stamp);
            self.api.send_input(&[mv, dn])?;
            std::thread::sleep(Duration::from_millis(50));
            for dest in destinations {
                self.check()?;
                if self.api.get_foreground_window() != target {
                    return Err(KaxError::blocked("foreground_changed"));
                }
                moved = dest;
                let mv = self.api.mouse_move_event(dest.0, dest.1, stamp)?;
                self.api.send_input(&[mv])?;
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(())
        })();

        let up = self.api.mouse_button_event(4, stamp);
        let _ = self.api.send_input(&[up]);
        std::thread::sleep(Duration::from_millis(60));
        self.restore_cursor(cursor, moved);
        res?;
        self.check()
    }

    pub fn restore(&mut self) -> Result<()> {
        if self.restored {
            return Ok(());
        }
        self.restored = true;
        let Some(target) = self.target else {
            return Ok(());
        };
        if self.api.get_foreground_window() != target {
            return Ok(());
        }
        let Ok(last) = self.api.last_input() else {
            return Ok(());
        };
        if !self.allowed_ticks.contains(&last) {
            return Ok(());
        }
        if let Some(tf) = self.target_focus {
            if self.api.focus_hwnd(target).ok() != Some(tf) && self.api.is_window_visible(tf) {
                let _ = self.api.focus_child(tf);
            }
        }
        if self.initial != 0 && self.initial != target && self.api.is_window(self.initial) {
            if !self.switch(self.initial) {
                return Err(KaxError::blocked("foreground_restore_failed"));
            }
            if let Some(ifocus) = self.initial_focus {
                if self.api.is_window_visible(ifocus) {
                    if self.api.focus_hwnd(self.initial).ok() != Some(ifocus) {
                        let _ = self.api.focus_child(ifocus);
                    }
                }
            }
        }
        if self.was_minimized && self.api.is_window(target) {
            let cmd = if self.api.get_foreground_window() != target {
                7 // SW_SHOWMINNOACTIVE
            } else {
                6 // SW_MINIMIZE
            };
            self.api.show_window_async(target, cmd);
        }
        Ok(())
    }

    /// Run `f` with `hwnd` activated in the foreground, then restore the previous
    /// foreground window and propagate any restore error (matching Python's
    /// `with foreground(api, hwnd):` `finally: guard.restore()`).
    pub fn with_activated<T, F>(api: &'a A, hwnd: usize, f: F) -> Result<T>
    where
        F: FnOnce(&mut Self) -> Result<T>,
    {
        let mut guard = Self::activate_new(api, hwnd)?;
        let body_res = f(&mut guard);
        guard.restore()?;
        body_res
    }
}

impl<'a, A: Win32Api> Drop for ForegroundGuard<'a, A> {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

/// Low-level Win32 clipboard system operations (implemented live on Windows or mocked in tests).
pub trait ClipboardSys: Send + Sync {
    fn create_owner_window(&self) -> usize;
    fn destroy_window(&self, hwnd: usize);
    fn open_clipboard(&self, hwnd: usize) -> bool;
    fn close_clipboard(&self);
    fn empty_clipboard(&self) -> bool;
    fn get_sequence_number(&self) -> u32;
    fn get_owner(&self) -> usize;
    fn enum_formats(&self) -> Result<Vec<u32>>;
    fn get_clipboard_data(&self, format: u32) -> usize;
    fn set_clipboard_data(&self, format: u32, handle: usize) -> bool;
    fn duplicate_format(&self, format: u32, saved: &HashMap<u32, usize>) -> Result<Option<usize>>;
    fn free_format(&self, format: u32, handle: usize);
    fn read_unicode_bytes(&self, handle: usize) -> Result<Vec<u8>>;
}

/// High-level clipboard copy/restore contract used by `WindowsAx`.
pub trait ClipboardBackend<A: Win32Api>: Send + Sync {
    fn copy_chat(
        &self,
        pid: u32,
        perform: &mut dyn FnMut(&mut ForegroundGuard<'_, A>) -> Result<()>,
        guard: &mut ForegroundGuard<'_, A>,
        check_target: &mut dyn FnMut() -> Result<()>,
    ) -> Result<String>;
}

pub struct Clipboard<A: Win32Api, S: ClipboardSys> {
    pub api: std::sync::Arc<A>,
    pub sys: S,
    pub copy_wait_secs: f64,
}

impl<A: Win32Api, S: ClipboardSys> Clipboard<A, S> {
    pub fn new(api: std::sync::Arc<A>, sys: S) -> Self {
        Self {
            api,
            sys,
            copy_wait_secs: 1.25,
        }
    }

    fn open_guard(&self, hwnd: usize) -> Result<ClipboardOpenGuard<'_, S>> {
        let opened = until_bool(
            || {
                self.api.pump_messages();
                self.sys.open_clipboard(hwnd)
            },
            1.0,
            0.025,
        );
        if !opened {
            return Err(KaxError::blocked("clipboard_busy"));
        }
        Ok(ClipboardOpenGuard { sys: &self.sys })
    }

    fn sequence(&self) -> Result<u32> {
        let n = self.sys.get_sequence_number();
        if n == 0 {
            return Err(KaxError::blocked("clipboard_sequence_unavailable"));
        }
        Ok(n)
    }

    fn restore(
        &self,
        hwnd: usize,
        pid: u32,
        old_sequence: u32,
        copied_sequence: Option<u32>,
        saved: &mut HashMap<u32, usize>,
        saved_order: &[u32],
    ) -> Result<()> {
        let _open = self.open_guard(hwnd)?;
        let sequence = self.sequence()?;
        let owner = self.sys.get_owner();
        if sequence == old_sequence {
            return Ok(());
        }
        if owner == 0
            || self.api.pid(owner) != pid
            || copied_sequence.is_some_and(|cs| sequence != cs)
        {
            return Err(KaxError::blocked("clipboard_changed"));
        }
        if !self.sys.empty_clipboard() {
            return Err(KaxError::blocked("clipboard_restore_failed"));
        }
        let mut missing = false;
        for &fmt in saved_order {
            if let Some(&handle) = saved.get(&fmt) {
                if self.sys.set_clipboard_data(fmt, handle) {
                    saved.remove(&fmt);
                } else {
                    missing = true;
                }
            }
        }
        if missing {
            return Err(KaxError::blocked("clipboard_restore_failed"));
        }
        Ok(())
    }
}

struct ClipboardOpenGuard<'a, S: ClipboardSys> {
    sys: &'a S,
}

impl<'a, S: ClipboardSys> Drop for ClipboardOpenGuard<'a, S> {
    fn drop(&mut self) {
        self.sys.close_clipboard();
    }
}

impl<A: Win32Api, S: ClipboardSys> ClipboardBackend<A> for Clipboard<A, S> {
    fn copy_chat(
        &self,
        pid: u32,
        perform: &mut dyn FnMut(&mut ForegroundGuard<'_, A>) -> Result<()>,
        guard: &mut ForegroundGuard<'_, A>,
        check_target: &mut dyn FnMut() -> Result<()>,
    ) -> Result<String> {
        let hwnd = self.sys.create_owner_window();
        if hwnd == 0 {
            return Err(KaxError::blocked("clipboard_window_unavailable"));
        }
        let mut saved: HashMap<u32, usize> = HashMap::new();
        let mut saved_order: Vec<u32> = Vec::new();
        let mut old_sequence: Option<u32> = None;
        let mut copied_sequence: Option<u32> = None;
        let mut attempted = false;

        let body_result: Result<String> = (|| {
            guard.check()?;
            {
                let _open = self.open_guard(hwnd)?;
                for fmt in self.sys.enum_formats()? {
                    if let Some(handle) = self.sys.duplicate_format(fmt, &saved)? {
                        if saved.insert(fmt, handle).is_none() {
                            saved_order.push(fmt);
                        }
                    }
                }
                old_sequence = Some(self.sequence()?);
            }
            let old_seq = old_sequence.unwrap();
            guard.check()?;
            if self.sequence()? != old_seq {
                return Err(KaxError::blocked("clipboard_changed"));
            }
            attempted = true;
            perform(guard)?;

            let check_new_owner = |g: &ForegroundGuard<'_, A>| -> Result<bool> {
                self.api.pump_messages();
                g.check()?;
                let owner = self.sys.get_owner();
                let changed =
                    owner != 0 && self.api.pid(owner) == pid && self.sequence()? != old_seq;
                g.check()?;
                Ok(changed)
            };

            let observed = until(
                || match check_new_owner(guard) {
                    Ok(true) => Some(Ok(())),
                    Ok(false) => None,
                    Err(e) => Some(Err(e)),
                },
                self.copy_wait_secs,
                0.025,
            );
            match observed {
                Some(Ok(())) => {}
                Some(Err(e)) => return Err(e),
                None => return Err(KaxError::blocked("clipboard_copy_not_observed")),
            }

            {
                let _open = self.open_guard(hwnd)?;
                if !check_new_owner(guard)? {
                    return Err(KaxError::blocked("clipboard_changed"));
                }
                copied_sequence = Some(self.sequence()?);
                let handle = self.sys.get_clipboard_data(CF_UNICODETEXT);
                copied_sequence = Some(self.sequence()?);
                if handle == 0 {
                    return Err(KaxError::blocked("clipboard_unicode_text_unavailable"));
                }
                let data = self.sys.read_unicode_bytes(handle)?;
                let text = decode_utf16_nul_terminated(&data)?;
                guard.check()?;
                check_target()?;
                Ok(text)
            }
        })();

        let mut restore_err: Option<KaxError> = None;
        if attempted {
            if let Some(old_seq) = old_sequence {
                let can_restore = if copied_sequence.is_none() {
                    guard.check().is_ok()
                } else {
                    true
                };
                if can_restore {
                    if let Err(err) = self.restore(
                        hwnd,
                        pid,
                        old_seq,
                        copied_sequence,
                        &mut saved,
                        &saved_order,
                    ) {
                        restore_err = Some(err);
                    }
                }
            }
        }

        for (fmt, handle) in saved.drain() {
            self.sys.free_format(fmt, handle);
        }
        self.sys.destroy_window(hwnd);

        match (body_result, restore_err) {
            (Ok(text), None) => Ok(text),
            (Ok(_), Some(rerr)) => Err(rerr),
            (Err(KaxError::Blocked(ref b)), Some(rerr)) if b == "clipboard_copy_not_observed" => {
                // If an external process modified the clipboard while we waited,
                // report `clipboard_changed` just like Python's `finally: restore()` does.
                Err(rerr)
            }
            (Err(err), _) => Err(err),
        }
    }
}

pub fn decode_utf16_nul_terminated(data: &[u8]) -> Result<String> {
    let mut end = None;
    let mut i = 0;
    while i + 1 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 {
            end = Some(i);
            break;
        }
        i += 2;
    }
    let Some(end_idx) = end else {
        return Err(KaxError::blocked("clipboard_unicode_text_invalid"));
    };
    let units: Vec<u16> = data[..end_idx]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16(&units)
        .map_err(|_| KaxError::blocked("clipboard_unicode_text_invalid"))
}

pub fn data_type_for_format(format: u32) -> u32 {
    match format {
        0x82 => 2,
        0x83 => 3,
        0x8E => 14,
        other => other,
    }
}

pub fn is_synthesized_equivalent_saved(format: u32, saved: &HashMap<u32, usize>) -> bool {
    const GROUPS: &[&[u32]] = &[&[1, 7, 13], &[2, 8, 17], &[3, 14]];
    GROUPS
        .iter()
        .any(|g| g.contains(&format) && g.iter().any(|f| saved.contains_key(f)))
}
