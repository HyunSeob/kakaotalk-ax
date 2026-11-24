//! Live Windows Win32 API and Clipboard system bindings via `windows-sys`.

#![cfg(target_os = "windows")]

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, GlobalFree, SetLastError, BOOL, FILETIME, HWND, LPARAM, POINT,
    RECT, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{
    ClientToScreen, CopyEnhMetaFileW, CreateCompatibleDC, CreateDIBSection, DeleteDC,
    DeleteEnhMetaFile, DeleteMetaFile, DeleteObject, GetDC, ReleaseDC, ScreenToClient,
    SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HENHMETAFILE, HMETAFILE,
};
use windows_sys::Win32::Storage::Xps::PrintWindow;
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData, GetClipboardOwner,
    GetClipboardSequenceNumber, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows_sys::Win32::System::Ole::OleDuplicateData;
use windows_sys::Win32::System::SystemInformation::GetTickCount;
use windows_sys::Win32::System::Threading::{
    AttachThreadInput, GetCurrentThreadId, GetProcessTimes, OpenProcess,
    QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::HiDpi::{
    GetDpiForWindow, SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetLastInputInfo, IsWindowEnabled, SendInput, SetFocus, INPUT, INPUT_0,
    INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, LASTINPUTINFO, MOUSEINPUT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, CreateWindowExW, DestroyWindow, EnumChildWindows, EnumWindows, GetAncestor,
    GetClassNameW, GetClientRect, GetCursorPos, GetDlgCtrlID, GetForegroundWindow,
    GetGUIThreadInfo, GetParent, GetSystemMetrics, GetWindow, GetWindowLongW, GetWindowRect, GetWindowTextLengthW,
    GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, PeekMessageW,
    PostMessageW, SendMessageTimeoutW, SetForegroundWindow, ShowWindowAsync, WindowFromPoint,
    GWL_STYLE, GWL_EXSTYLE, WS_POPUP, WS_VISIBLE, WS_CLIPSIBLINGS, WS_EX_TOOLWINDOW, WS_EX_LAYERED,
    GA_ROOT, GUITHREADINFO, GW_OWNER, MSG, PM_NOREMOVE, SMTO_ABORTIFHUNG, SMTO_BLOCK,
    SMTO_ERRORONEXIT, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use crate::error::{KaxError, Result};
use crate::win32_api::{
    activate_foreground_with_ops, is_idle_tick, packed, send_input_with_ops, until, DpiGuard,
    ForegroundUnlockOps, InputEvent, Win32Api, EM_REPLACESEL, EM_SETSEL, MODIFIER_KEYS,
    PW_RENDERFULLCONTENT, VK_END, VK_HOME, VK_PRIOR, VK_UNASSIGNED, WM_GETTEXT, WM_GETTEXTLENGTH,
    WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NULL,
    WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETTEXT,
};
use crate::win32_clipboard::{
    data_type_for_format, is_synthesized_equivalent_saved, ClipboardSys,
};
use crate::win32_image::Frame;

pub struct Win32Live {
    own_ticks: Mutex<VecDeque<u32>>,
}

impl Win32Live {
    pub fn new() -> Self {
        Self {
            own_ticks: Mutex::new(VecDeque::with_capacity(32)),
        }
    }

    fn send_message_timeout(
        &self,
        hwnd: usize,
        code: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Result<usize> {
        let mut result: usize = 0;
        let ok = unsafe {
            SendMessageTimeoutW(
                hwnd as HWND,
                code,
                wparam,
                lparam,
                SMTO_BLOCK | SMTO_ABORTIFHUNG | SMTO_ERRORONEXIT,
                500,
                &mut result,
            )
        };
        if ok == 0 {
            return Err(KaxError::blocked("win32_message_timeout"));
        }
        Ok(result)
    }
}

impl Default for Win32Live {
    fn default() -> Self {
        Self::new()
    }
}

unsafe extern "system" fn enum_windows_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let list = &mut *(lparam as *mut Vec<usize>);
    list.push(hwnd as usize);
    1
}

impl Win32Api for Win32Live {
    fn handles(&self, parent: Option<usize>) -> Vec<usize> {
        let mut found: Vec<usize> = Vec::new();
        let ptr = (&mut found as *mut Vec<usize>) as LPARAM;
        unsafe {
            match parent {
                None => {
                    EnumWindows(Some(enum_windows_proc), ptr);
                }
                Some(p) => {
                    EnumChildWindows(p as HWND, Some(enum_windows_proc), ptr);
                }
            }
        }
        found
    }

    fn title(&self, hwnd: usize) -> Result<String> {
        let length = unsafe { GetWindowTextLengthW(hwnd as HWND) };
        if length < 0 {
            return Ok(String::new());
        }
        if length > 32767 {
            return Err(KaxError::blocked("window_title_too_long"));
        }
        let mut buf = vec![0u16; (length as usize) + 2];
        let n = unsafe { GetWindowTextW(hwnd as HWND, buf.as_mut_ptr(), buf.len() as i32) };
        if n as usize == buf.len() - 1 {
            return Err(KaxError::blocked("window_title_changed"));
        }
        let slice = &buf[..(n.max(0) as usize)];
        Ok(OsString::from_wide(slice).to_string_lossy().into_owned())
    }

    fn class_name(&self, hwnd: usize) -> String {
        let mut buf = [0u16; 256];
        let n = unsafe { GetClassNameW(hwnd as HWND, buf.as_mut_ptr(), buf.len() as i32) };
        if n <= 0 {
            return String::new();
        }
        OsString::from_wide(&buf[..n as usize])
            .to_string_lossy()
            .into_owned()
    }

    fn dlg_ctrl_id(&self, hwnd: usize) -> i32 {
        unsafe { GetDlgCtrlID(hwnd as HWND) }
    }

    fn pid(&self, hwnd: usize) -> u32 {
        let mut pid = 0u32;
        unsafe {
            GetWindowThreadProcessId(hwnd as HWND, &mut pid);
        }
        pid
    }

    fn process(&self, pid: u32) -> (String, u64) {
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            return (String::new(), 0);
        }
        let mut buf = vec![0u16; 32768];
        let mut size = buf.len() as u32;
        let ok = unsafe { QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut size) };
        let result = if ok != 0 {
            let full = OsString::from_wide(&buf[..size as usize])
                .to_string_lossy()
                .into_owned();
            let mut creation = FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            };
            let mut exit = FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            };
            let mut kernel = FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            };
            let mut user = FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            };
            let has_times =
                unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) };
            let created = if has_times != 0 {
                ((creation.dwHighDateTime as u64) << 32) | (creation.dwLowDateTime as u64)
            } else {
                0
            };
            let base = Path::new(&full)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_lowercase();
            (base, created)
        } else {
            (String::new(), 0)
        };
        unsafe { CloseHandle(handle) };
        result
    }

    fn is_window(&self, hwnd: usize) -> bool {
        unsafe { IsWindow(hwnd as HWND) != 0 }
    }

    fn is_window_visible(&self, hwnd: usize) -> bool {
        unsafe { IsWindowVisible(hwnd as HWND) != 0 }
    }

    fn is_window_enabled(&self, hwnd: usize) -> bool {
        unsafe { IsWindowEnabled(hwnd as HWND) != 0 }
    }

    fn is_iconic(&self, hwnd: usize) -> bool {
        unsafe { IsIconic(hwnd as HWND) != 0 }
    }

    fn get_parent(&self, hwnd: usize) -> usize {
        unsafe { GetParent(hwnd as HWND) as usize }
    }

    fn get_owner(&self, hwnd: usize) -> usize {
        unsafe { GetWindow(hwnd as HWND, GW_OWNER) as usize }
    }

    fn has_balloon_styles(&self, hwnd: usize) -> bool {
        let style = unsafe { GetWindowLongW(hwnd as HWND, GWL_STYLE) } as u32;
        let ex_style = unsafe { GetWindowLongW(hwnd as HWND, GWL_EXSTYLE) } as u32;
        style == (WS_POPUP | WS_VISIBLE | WS_CLIPSIBLINGS)
            && ex_style == (WS_EX_TOOLWINDOW | WS_EX_LAYERED)
    }

    fn get_ancestor_root(&self, hwnd: usize) -> usize {
        unsafe { GetAncestor(hwnd as HWND, GA_ROOT) as usize }
    }

    fn get_dpi_for_window(&self, hwnd: usize) -> u32 {
        unsafe { GetDpiForWindow(hwnd as HWND) }
    }

    fn get_foreground_window(&self) -> usize {
        unsafe { GetForegroundWindow() as usize }
    }

    fn set_foreground_window(&self, hwnd: usize) -> bool {
        unsafe { SetForegroundWindow(hwnd as HWND) != 0 }
    }

    fn bring_window_to_top(&self, hwnd: usize) -> bool {
        unsafe { BringWindowToTop(hwnd as HWND) != 0 }
    }

    fn show_window_async(&self, hwnd: usize, cmd: i32) -> bool {
        unsafe { ShowWindowAsync(hwnd as HWND, cmd) != 0 }
    }

    fn get_async_key_state(&self, vk: i32) -> i16 {
        unsafe { GetAsyncKeyState(vk) }
    }

    fn window_from_point(&self, x: i32, y: i32) -> usize {
        unsafe { WindowFromPoint(POINT { x, y }) as usize }
    }

    fn control_text(&self, hwnd: usize) -> Result<String> {
        let length = self.send_message_timeout(hwnd, WM_GETTEXTLENGTH, 0, 0)?;
        if length > 65536 {
            return Err(KaxError::blocked("control_text_too_long"));
        }
        let mut buf = vec![0u16; length + 2];
        let n = self.send_message_timeout(
            hwnd,
            WM_GETTEXT,
            buf.len(),
            buf.as_mut_ptr() as LPARAM,
        )?;
        if n == buf.len() - 1 {
            return Err(KaxError::blocked("control_text_changed"));
        }
        let nul_pos = buf[..n].iter().position(|&c| c == 0).unwrap_or(n);
        Ok(OsString::from_wide(&buf[..nul_pos])
            .to_string_lossy()
            .into_owned())
    }

    fn set_text(&self, hwnd: usize, value: &str) -> Result<()> {
        let wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
        let res = self.send_message_timeout(hwnd, WM_SETTEXT, 0, wide.as_ptr() as LPARAM)?;
        if res == 0 {
            return Err(KaxError::blocked("control_text_rejected"));
        }
        Ok(())
    }

    fn replace_edit(&self, hwnd: usize, value: &str) -> Result<()> {
        self.send_message_timeout(hwnd, EM_SETSEL, 0, -1)?;
        let wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
        self.send_message_timeout(hwnd, EM_REPLACESEL, 1, wide.as_ptr() as LPARAM)?;
        Ok(())
    }

    fn post(&self, hwnd: usize, code: u32, wparam: usize, lparam: isize) -> Result<()> {
        let ok = unsafe { PostMessageW(hwnd as HWND, code, wparam, lparam) };
        if ok == 0 {
            return Err(KaxError::blocked("win32_message_rejected"));
        }
        Ok(())
    }

    fn click(&self, hwnd: usize, x: i32, y: i32, double: bool, right: bool) -> Result<()> {
        let pos = packed(x, y);
        if right {
            self.post(hwnd, WM_RBUTTONDOWN, 2, pos)?;
            self.post(hwnd, WM_RBUTTONUP, 0, pos)?;
            return Ok(());
        }
        self.post(hwnd, WM_LBUTTONDOWN, 1, pos)?;
        self.post(hwnd, WM_LBUTTONUP, 0, pos)?;
        if double {
            self.post(hwnd, WM_LBUTTONDBLCLK, 1, pos)?;
            self.post(hwnd, WM_LBUTTONUP, 0, pos)?;
        }
        Ok(())
    }

    fn wheel(&self, hwnd: usize, delta: i32) -> Result<()> {
        let (_, _, w, h) = self.rect(hwnd, true)?;
        let (x, y) = self.point(hwnd, w / 2, h / 2, true)?;
        self.post(hwnd, WM_MOUSEWHEEL, packed(0, delta) as usize, packed(x, y))
    }

    fn tick(&self) -> u32 {
        unsafe { GetTickCount() }
    }

    fn last_input(&self) -> Result<u32> {
        let mut info = LASTINPUTINFO {
            cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        if unsafe { GetLastInputInfo(&mut info) } == 0 {
            return Err(KaxError::blocked("user_input_status_unavailable"));
        }
        Ok(info.dwTime)
    }

    fn idle(&self, milliseconds: u32) -> Result<u32> {
        let found = until(
            || {
                let last = Win32Api::last_input(self).ok()?;
                let any_down = MODIFIER_KEYS
                    .iter()
                    .any(|&vk| (self.get_async_key_state(vk) as u16 & 0x8000) != 0);
                let ticks: Vec<u32> = self.own_ticks.lock().unwrap().iter().copied().collect();
                if is_idle_tick(Win32Api::tick(self), last, milliseconds, &ticks, any_down) {
                    Some(last)
                } else {
                    None
                }
            },
            0.9,
            0.025,
        );
        found.ok_or_else(|| KaxError::blocked("user_input_active"))
    }

    fn focus_hwnd(&self, hwnd: usize) -> Result<usize> {
        let thread = if hwnd != 0 {
            unsafe { GetWindowThreadProcessId(hwnd as HWND, std::ptr::null_mut()) }
        } else {
            0
        };
        let mut info: GUITHREADINFO = unsafe { std::mem::zeroed() };
        info.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
        if unsafe { GetGUIThreadInfo(thread, &mut info) } == 0 {
            return Err(KaxError::blocked("focus_unavailable"));
        }
        Ok(info.hwndFocus as usize)
    }

    fn focus_child(&self, hwnd: usize) -> Result<()> {
        let target = unsafe { GetWindowThreadProcessId(hwnd as HWND, std::ptr::null_mut()) };
        let current = unsafe { GetCurrentThreadId() };
        let attached = current != target;
        if attached {
            Win32Api::pump_messages(self);
            if unsafe { AttachThreadInput(current, target, 1) } == 0 {
                return Err(KaxError::blocked("focus_unavailable"));
            }
        }
        let res = (|| -> Result<()> {
            unsafe { SetFocus(hwnd as HWND) };
            if self.focus_hwnd(hwnd)? != hwnd {
                return Err(KaxError::blocked("focus_unavailable"));
            }
            Ok(())
        })();
        if attached {
            unsafe { AttachThreadInput(current, target, 0) };
        }
        res
    }

    fn activate_foreground(
        &self,
        hwnd: usize,
        allowed_ticks: Option<&mut HashSet<u32>>,
        seconds: f64,
    ) -> bool {
        activate_foreground_with_ops(self, hwnd, allowed_ticks, seconds)
    }

    fn pump_messages(&self) {
        let mut msg: MSG = unsafe { std::mem::zeroed() };
        unsafe {
            PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_NOREMOVE);
        }
    }

    fn input_keys(&self, keys: &[(u16, bool)], stamp: u32) -> Result<()> {
        let events: Vec<InputEvent> = keys
            .iter()
            .map(|&(key, up)| {
                let mut flags = if up { 2u32 } else { 0u32 };
                if matches!(key, VK_HOME | VK_PRIOR | VK_END) {
                    flags |= 1;
                }
                InputEvent::Key {
                    vk: key,
                    flags,
                    timestamp: stamp,
                }
            })
            .collect();
        self.send_input(&events)
    }

    fn send_input(&self, events: &[InputEvent]) -> Result<()> {
        let mut raw: Vec<INPUT> = Vec::with_capacity(events.len());
        for &ev in events {
            match ev {
                InputEvent::Key {
                    vk,
                    flags,
                    timestamp,
                } => {
                    raw.push(INPUT {
                        r#type: INPUT_KEYBOARD,
                        Anonymous: INPUT_0 {
                            ki: KEYBDINPUT {
                                wVk: vk,
                                wScan: 0,
                                dwFlags: flags,
                                time: timestamp,
                                dwExtraInfo: 0,
                            },
                        },
                    });
                }
                InputEvent::Mouse {
                    x,
                    y,
                    flags,
                    timestamp,
                } => {
                    raw.push(INPUT {
                        r#type: INPUT_MOUSE,
                        Anonymous: INPUT_0 {
                            mi: MOUSEINPUT {
                                dx: x,
                                dy: y,
                                mouseData: 0,
                                dwFlags: flags,
                                time: timestamp,
                                dwExtraInfo: 0,
                            },
                        },
                    });
                }
            }
        }
        send_input_with_ops(events, &self.own_ticks, |_| {
            let sent = unsafe {
                SendInput(
                    raw.len() as u32,
                    raw.as_ptr(),
                    std::mem::size_of::<INPUT>() as i32,
                )
            };
            if sent as usize != raw.len() {
                return Err(KaxError::blocked("keyboard_input_rejected"));
            }
            Ok(())
        })
    }

    fn cursor(&self) -> Result<(i32, i32)> {
        let mut pt = POINT { x: 0, y: 0 };
        if unsafe { GetCursorPos(&mut pt) } == 0 {
            return Err(KaxError::blocked("cursor_unavailable"));
        }
        Ok((pt.x, pt.y))
    }

    fn mouse_click(&self, x: i32, y: i32, stamp: u32) -> Result<()> {
        let mv = self.mouse_move_event(x, y, stamp)?;
        let dn = self.mouse_button_event(2, stamp);
        let up = self.mouse_button_event(4, stamp);
        self.send_input(&[mv, dn, up])
    }

    fn mouse_move_event(&self, x: i32, y: i32, stamp: u32) -> Result<InputEvent> {
        let left = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
        let top = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
        let width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
        let height = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
        if !(x >= left && x < left + width && y >= top && y < top + height) {
            return Err(KaxError::blocked("click_target_off_screen"));
        }
        let mx = ((x - left) * 65536 + width / 2) / width;
        let my = ((y - top) * 65536 + height / 2) / height;
        Ok(InputEvent::Mouse {
            x: mx,
            y: my,
            flags: 0xE001,
            timestamp: stamp,
        })
    }

    fn arm_list(&self, hwnd: usize) -> Result<()> {
        let (_, _, _, height) = self.rect(hwnd, true)?;
        let mut clicked = false;
        crate::win32_api::arm_list_with_ops(hwnd, height, |h, x, y| {
            clicked = true;
            self.click(h, x, y, false, false)
        })?;
        if clicked {
            std::thread::sleep(Duration::from_millis(40));
        }
        Ok(())
    }

    fn clear_selection(&self, hwnd: usize) -> Result<()> {
        let (_, _, _, height) = self.rect(hwnd, true)?;
        if height < 40 {
            return Ok(());
        }
        self.post(hwnd, WM_LBUTTONDOWN, 1, packed(5, 7))?;
        std::thread::sleep(Duration::from_millis(40));
        self.post(hwnd, WM_MOUSEMOVE, 1, packed(5, 33))?;
        std::thread::sleep(Duration::from_millis(40));
        self.post(hwnd, WM_LBUTTONUP, 0, packed(5, 33))?;
        self.click(hwnd, 5, height - 8, false, false)?;
        std::thread::sleep(Duration::from_millis(30));
        Ok(())
    }

    fn rect(&self, hwnd: usize, client: bool) -> Result<(i32, i32, i32, i32)> {
        let mut r = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        let ok = if client {
            unsafe { GetClientRect(hwnd as HWND, &mut r) }
        } else {
            unsafe { GetWindowRect(hwnd as HWND, &mut r) }
        };
        if ok == 0 {
            return Err(KaxError::blocked("window_geometry_unavailable"));
        }
        Ok((r.left, r.top, r.right, r.bottom))
    }

    fn point(&self, hwnd: usize, x: i32, y: i32, to_screen: bool) -> Result<(i32, i32)> {
        let mut pt = POINT { x, y };
        let ok = if to_screen {
            unsafe { ClientToScreen(hwnd as HWND, &mut pt) }
        } else {
            unsafe { ScreenToClient(hwnd as HWND, &mut pt) }
        };
        if ok == 0 {
            return Err(KaxError::blocked("window_geometry_unavailable"));
        }
        Ok((pt.x, pt.y))
    }

    fn dpi_aware(&self) -> DpiGuard {
        let prior = unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        if prior.is_null() {
            DpiGuard::noop()
        } else {
            let prior_addr = prior as usize;
            DpiGuard::new(move || unsafe {
                SetThreadDpiAwarenessContext(prior_addr as _);
            })
        }
    }

    fn capture(&self, hwnd: usize) -> Result<Frame> {
        self.send_message_timeout(hwnd, WM_NULL, 0, 0)?;
        let (left, top, right, bottom) = self.rect(hwnd, false)?;
        let width = right - left;
        let height = bottom - top;
        if width <= 0 || height <= 0 || (width as i64) * (height as i64) > 20_000_000 {
            return Err(KaxError::blocked("capture_geometry_invalid"));
        }
        let w = width as usize;
        let h = height as usize;
        let dc = unsafe { GetDC(std::ptr::null_mut()) };
        if dc.is_null() {
            return Err(KaxError::blocked("window_capture_failed"));
        }
        let memory = unsafe { CreateCompatibleDC(dc) };
        let res = (|| -> Result<Frame> {
            let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    biSizeImage: 0,
                    biXPelsPerMeter: 0,
                    biYPelsPerMeter: 0,
                    biClrUsed: 0,
                    biClrImportant: 0,
                },
                bmiColors: [unsafe { std::mem::zeroed() }; 1],
            };
            let bitmap = unsafe {
                CreateDIBSection(
                    dc,
                    &info,
                    DIB_RGB_COLORS,
                    &mut bits,
                    std::ptr::null_mut(),
                    0,
                )
            };
            if memory.is_null() || bitmap.is_null() || bits.is_null() {
                if !bitmap.is_null() {
                    unsafe { DeleteObject(bitmap) };
                }
                return Err(KaxError::blocked("window_capture_failed"));
            }
            let previous = unsafe { SelectObject(memory, bitmap) };
            let cap = (|| -> Result<Frame> {
                let total_bytes = w * h * 4;
                unsafe {
                    std::ptr::write_bytes(bits.cast::<u8>(), 0, total_bytes);
                }
                if unsafe { PrintWindow(hwnd as HWND, memory, PW_RENDERFULLCONTENT) } == 0 {
                    return Err(KaxError::blocked("window_capture_failed"));
                }
                let slice = unsafe { std::slice::from_raw_parts(bits.cast::<u8>(), total_bytes) };
                let non_blank = slice
                    .chunks_exact(4)
                    .any(|px| px[0] != 0 || px[1] != 0 || px[2] != 0);
                if !non_blank {
                    return Err(KaxError::blocked("window_capture_blank"));
                }
                Ok(Frame::new(w, h, slice.to_vec()))
            })();
            if !previous.is_null() {
                unsafe { SelectObject(memory, previous) };
            }
            unsafe { DeleteObject(bitmap) };
            cap
        })();
        if !memory.is_null() {
            unsafe { DeleteDC(memory) };
        }
        unsafe { ReleaseDC(std::ptr::null_mut(), dc) };
        res
    }
}

impl ForegroundUnlockOps for Win32Live {
    fn is_window(&self, hwnd: usize) -> bool {
        Win32Api::is_window(self, hwnd)
    }
    fn is_iconic(&self, hwnd: usize) -> bool {
        Win32Api::is_iconic(self, hwnd)
    }
    fn get_foreground_window(&self) -> usize {
        Win32Api::get_foreground_window(self)
    }
    fn pump_messages(&self) {
        Win32Api::pump_messages(self);
    }
    fn current_thread_id(&self) -> u32 {
        unsafe { GetCurrentThreadId() }
    }
    fn window_thread_id(&self, hwnd: usize) -> u32 {
        unsafe { GetWindowThreadProcessId(hwnd as HWND, std::ptr::null_mut()) }
    }
    fn attach_thread_input(&self, current: u32, target: u32, attach: bool) -> bool {
        unsafe { AttachThreadInput(current, target, i32::from(attach)) != 0 }
    }
    fn bring_window_to_top(&self, hwnd: usize) {
        Win32Api::bring_window_to_top(self, hwnd);
    }
    fn set_foreground_window(&self, hwnd: usize) {
        Win32Api::set_foreground_window(self, hwnd);
    }
    fn any_modifier_down(&self) -> bool {
        MODIFIER_KEYS
            .iter()
            .any(|&vk| (self.get_async_key_state(vk) as u16 & 0x8000) != 0)
    }
    fn last_input(&self) -> Result<u32> {
        Win32Api::last_input(self)
    }
    fn tick(&self) -> u32 {
        Win32Api::tick(self)
    }
    fn send_unlock_key(&self, stamp: u32) -> Result<()> {
        self.send_input(&[InputEvent::Key {
            vk: VK_UNASSIGNED,
            flags: 2,
            timestamp: stamp,
        }])
    }
}

#[repr(C)]
struct MetaFilePict {
    mode: i32,
    x: i32,
    y: i32,
    metafile: HMETAFILE,
}

pub struct LiveClipboardSys;

impl ClipboardSys for LiveClipboardSys {
    fn create_owner_window(&self) -> usize {
        let cls: Vec<u16> = "STATIC\0".encode_utf16().collect();
        unsafe {
            CreateWindowExW(
                0,
                cls.as_ptr(),
                std::ptr::null(),
                0,
                0,
                0,
                0,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            ) as usize
        }
    }

    fn destroy_window(&self, hwnd: usize) {
        unsafe {
            DestroyWindow(hwnd as HWND);
        }
    }

    fn open_clipboard(&self, hwnd: usize) -> bool {
        unsafe { OpenClipboard(hwnd as HWND) != 0 }
    }

    fn close_clipboard(&self) {
        unsafe {
            CloseClipboard();
        }
    }

    fn empty_clipboard(&self) -> bool {
        unsafe { EmptyClipboard() != 0 }
    }

    fn get_sequence_number(&self) -> u32 {
        unsafe { GetClipboardSequenceNumber() }
    }

    fn get_owner(&self) -> usize {
        unsafe { GetClipboardOwner() as usize }
    }

    fn enum_formats(&self) -> Result<Vec<u32>> {
        let mut formats = Vec::new();
        let mut fmt = 0u32;
        loop {
            unsafe { SetLastError(0) };
            fmt = unsafe { EnumClipboardFormats(fmt) };
            if fmt == 0 {
                if unsafe { GetLastError() } != 0 {
                    return Err(KaxError::blocked("clipboard_backup_failed"));
                }
                break;
            }
            formats.push(fmt);
        }
        Ok(formats)
    }

    fn get_clipboard_data(&self, format: u32) -> usize {
        unsafe { GetClipboardData(format) as usize }
    }

    fn set_clipboard_data(&self, format: u32, handle: usize) -> bool {
        unsafe { !SetClipboardData(format, handle as _).is_null() }
    }

    fn duplicate_format(&self, format: u32, saved: &HashMap<u32, usize>) -> Result<Option<usize>> {
        if format == 0x80 || (0x200..=0x3FF).contains(&format) {
            return Err(KaxError::blocked("clipboard_format_cannot_be_preserved"));
        }
        let handle = self.get_clipboard_data(format);
        if handle == 0 {
            if is_synthesized_equivalent_saved(format, saved) {
                return Ok(None);
            }
            return Err(KaxError::blocked("clipboard_backup_failed"));
        }
        let kind = data_type_for_format(format);
        let dup = if kind == 14 {
            unsafe { CopyEnhMetaFileW(handle as HENHMETAFILE, std::ptr::null()) as usize }
        } else {
            unsafe { OleDuplicateData(handle as _, kind as u16, 2) as usize }
        };
        if dup == 0 {
            return Err(KaxError::blocked("clipboard_backup_failed"));
        }
        Ok(Some(dup))
    }

    fn free_format(&self, format: u32, handle: usize) {
        let kind = data_type_for_format(format);
        unsafe {
            if matches!(kind, 2 | 9) {
                DeleteObject(handle as _);
            } else if kind == 14 {
                DeleteEnhMetaFile(handle as HENHMETAFILE);
            } else {
                if kind == 3 {
                    let addr = GlobalLock(handle as _);
                    if !addr.is_null() {
                        let mfp = &*(addr as *const MetaFilePict);
                        DeleteMetaFile(mfp.metafile);
                        GlobalUnlock(handle as _);
                    }
                }
                GlobalFree(handle as _);
            }
        }
    }

    fn read_unicode_bytes(&self, handle: usize) -> Result<Vec<u8>> {
        let size = unsafe { GlobalSize(handle as _) };
        if size == 0 || size > 512 * 1024 * 1024 {
            return Err(KaxError::blocked("clipboard_data_unavailable"));
        }
        let addr = unsafe { GlobalLock(handle as _) };
        if addr.is_null() {
            return Err(KaxError::blocked("clipboard_data_unavailable"));
        }
        let bytes = unsafe { std::slice::from_raw_parts(addr.cast::<u8>(), size).to_vec() };
        unsafe { GlobalUnlock(handle as _) };
        Ok(bytes)
    }
}
