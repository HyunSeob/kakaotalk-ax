use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use kakaotalk_ax::error::{KaxError, Result};
use kakaotalk_ax::win32_api::{DpiGuard, InputEvent, Win32Api};
use kakaotalk_ax::win32_clipboard::{Clipboard, ClipboardBackend, ClipboardSys, ForegroundGuard};
use kakaotalk_ax::win32_image::Frame;

struct MockApiState {
    fg: usize,
    focuses: HashMap<usize, usize>,
    last_input: u32,
    tick: u32,
    minimized: HashSet<usize>,
    show_calls: Vec<(usize, i32)>,
    input_keys_calls: usize,
    inactive_focus_zero: bool,
    block_activate: bool,
}

struct MockApi {
    st: Mutex<MockApiState>,
}

impl MockApi {
    fn new() -> Arc<Self> {
        let mut focuses = HashMap::new();
        focuses.insert(31, 310);
        focuses.insert(50, 500);
        Arc::new(Self {
            st: Mutex::new(MockApiState {
                fg: 31,
                focuses,
                last_input: 42,
                tick: 99,
                minimized: HashSet::new(),
                show_calls: Vec::new(),
                input_keys_calls: 0,
                inactive_focus_zero: false,
                block_activate: false,
            }),
        })
    }
}

impl Win32Api for MockApi {
    fn handles(&self, _parent: Option<usize>) -> Vec<usize> { Vec::new() }
    fn title(&self, _hwnd: usize) -> Result<String> { Ok(String::new()) }
    fn class_name(&self, _hwnd: usize) -> String { String::new() }
    fn dlg_ctrl_id(&self, _hwnd: usize) -> i32 { 0 }
    fn pid(&self, hwnd: usize) -> u32 { hwnd as u32 }
    fn process(&self, _pid: u32) -> (String, u64) { ("kakaotalk.exe".into(), 1) }
    fn is_window(&self, _hwnd: usize) -> bool { true }
    fn is_window_visible(&self, _hwnd: usize) -> bool { true }
    fn is_window_enabled(&self, _hwnd: usize) -> bool { true }
    fn is_iconic(&self, hwnd: usize) -> bool { self.st.lock().unwrap().minimized.contains(&hwnd) }
    fn get_parent(&self, _hwnd: usize) -> usize { 0 }
    fn get_owner(&self, _hwnd: usize) -> usize { 0 }
    fn get_ancestor_root(&self, hwnd: usize) -> usize { hwnd }
    fn get_dpi_for_window(&self, _hwnd: usize) -> u32 { 96 }
    fn get_foreground_window(&self) -> usize { self.st.lock().unwrap().fg }
    fn set_foreground_window(&self, hwnd: usize) -> bool {
        let mut st = self.st.lock().unwrap();
        if st.block_activate {
            return false;
        }
        st.fg = hwnd;
        true
    }
    fn bring_window_to_top(&self, _hwnd: usize) -> bool { true }
    fn show_window_async(&self, hwnd: usize, cmd: i32) -> bool {
        let mut st = self.st.lock().unwrap();
        st.show_calls.push((hwnd, cmd));
        if cmd == 9 {
            st.minimized.remove(&hwnd);
        } else if cmd == 7 || cmd == 6 {
            st.minimized.insert(hwnd);
        }
        true
    }
    fn get_async_key_state(&self, _vk: i32) -> i16 { 0 }
    fn window_from_point(&self, _x: i32, _y: i32) -> usize { 50 }
    fn control_text(&self, _hwnd: usize) -> Result<String> { Ok(String::new()) }
    fn set_text(&self, _hwnd: usize, _value: &str) -> Result<()> { Ok(()) }
    fn replace_edit(&self, _hwnd: usize, _value: &str) -> Result<()> { Ok(()) }
    fn post(&self, _hwnd: usize, _code: u32, _wparam: usize, _lparam: isize) -> Result<()> { Ok(()) }
    fn click(&self, _hwnd: usize, _x: i32, _y: i32, _double: bool, _right: bool) -> Result<()> { Ok(()) }
    fn wheel(&self, _hwnd: usize, _delta: i32) -> Result<()> { Ok(()) }
    fn tick(&self) -> u32 { self.st.lock().unwrap().tick }
    fn last_input(&self) -> Result<u32> { Ok(self.st.lock().unwrap().last_input) }
    fn idle(&self, _ms: u32) -> Result<u32> { Ok(self.st.lock().unwrap().last_input) }
    fn focus_hwnd(&self, hwnd: usize) -> Result<usize> {
        let st = self.st.lock().unwrap();
        if st.inactive_focus_zero && st.fg != hwnd {
            return Ok(0);
        }
        Ok(*st.focuses.get(&hwnd).unwrap_or(&0))
    }
    fn focus_child(&self, hwnd: usize) -> Result<()> {
        let mut st = self.st.lock().unwrap();
        let fg = st.fg;
        st.focuses.insert(fg, hwnd);
        Ok(())
    }
    fn activate_foreground(&self, hwnd: usize, allowed_ticks: Option<&mut HashSet<u32>>, _s: f64) -> bool {
        let mut st = self.st.lock().unwrap();
        if st.block_activate {
            return false;
        }
        if let Some(ticks) = allowed_ticks {
            let t = st.tick;
            ticks.insert(t);
            st.last_input = t;
        }
        st.fg = hwnd;
        true
    }
    fn pump_messages(&self) {}
    fn input_keys(&self, _keys: &[(u16, bool)], _stamp: u32) -> Result<()> {
        self.st.lock().unwrap().input_keys_calls += 1;
        Ok(())
    }
    fn send_input(&self, _events: &[InputEvent]) -> Result<()> { Ok(()) }
    fn cursor(&self) -> Result<(i32, i32)> { Ok((0, 0)) }
    fn mouse_click(&self, _x: i32, _y: i32, _stamp: u32) -> Result<()> { Ok(()) }
    fn mouse_move_event(&self, x: i32, y: i32, stamp: u32) -> Result<InputEvent> {
        Ok(InputEvent::Mouse { x, y, flags: 0, timestamp: stamp })
    }
    fn arm_list(&self, _hwnd: usize) -> Result<()> { Ok(()) }
    fn clear_selection(&self, _hwnd: usize) -> Result<()> { Ok(()) }
    fn rect(&self, _hwnd: usize, _client: bool) -> Result<(i32, i32, i32, i32)> { Ok((0, 0, 380, 420)) }
    fn point(&self, _hwnd: usize, x: i32, y: i32, _to_screen: bool) -> Result<(i32, i32)> { Ok((x, y)) }
    fn dpi_aware(&self) -> DpiGuard { DpiGuard::noop() }
    fn capture(&self, _hwnd: usize) -> Result<Frame> { Ok(Frame::new(10, 10, vec![0; 400])) }
}

struct MockClipState {
    owner: usize,
    seq: u32,
    restored: HashMap<u32, usize>,
    restored_order: Vec<u32>,
    empty_calls: usize,
    free_calls: usize,
    destroy_calls: usize,
    get_data_calls: usize,
    unicode_bytes: Vec<u8>,
    opaque_error: bool,
}

struct MockClipSys {
    st: Arc<Mutex<MockClipState>>,
}

impl MockClipSys {
    fn new() -> (Self, Arc<Mutex<MockClipState>>) {
        let mut utf16 = Vec::new();
        for unit in "원문 🧪\0".encode_utf16() {
            utf16.extend_from_slice(&unit.to_le_bytes());
        }
        utf16.extend_from_slice(&[0, 0]);
        let st = Arc::new(Mutex::new(MockClipState {
            owner: 10,
            seq: 1,
            restored: HashMap::new(),
            restored_order: Vec::new(),
            empty_calls: 0,
            free_calls: 0,
            destroy_calls: 0,
            get_data_calls: 0,
            unicode_bytes: utf16,
            opaque_error: false,
        }));
        (Self { st: Arc::clone(&st) }, st)
    }
}

impl ClipboardSys for MockClipSys {
    fn create_owner_window(&self) -> usize { 999 }
    fn destroy_window(&self, _hwnd: usize) { self.st.lock().unwrap().destroy_calls += 1; }
    fn open_clipboard(&self, _hwnd: usize) -> bool { true }
    fn close_clipboard(&self) {}
    fn empty_clipboard(&self) -> bool {
        self.st.lock().unwrap().empty_calls += 1;
        true
    }
    fn get_sequence_number(&self) -> u32 { self.st.lock().unwrap().seq }
    fn get_owner(&self) -> usize { self.st.lock().unwrap().owner }
    fn enum_formats(&self) -> Result<Vec<u32>> { Ok(vec![13, 8, 15, 0xC117]) }
    fn get_clipboard_data(&self, _format: u32) -> usize {
        self.st.lock().unwrap().get_data_calls += 1;
        701
    }
    fn set_clipboard_data(&self, format: u32, handle: usize) -> bool {
        let mut st = self.st.lock().unwrap();
        st.restored.insert(format, handle);
        st.restored_order.push(format);
        true
    }
    fn duplicate_format(&self, format: u32, _saved: &HashMap<u32, usize>) -> Result<Option<usize>> {
        if self.st.lock().unwrap().opaque_error {
            return Err(KaxError::blocked("clipboard_format_cannot_be_preserved"));
        }
        Ok(Some(format as usize + 500))
    }
    fn free_format(&self, _format: u32, _handle: usize) {
        self.st.lock().unwrap().free_calls += 1;
    }
    fn read_unicode_bytes(&self, _handle: usize) -> Result<Vec<u8>> {
        Ok(self.st.lock().unwrap().unicode_bytes.clone())
    }
}

fn make_clip() -> (Clipboard<MockApi, MockClipSys>, Arc<MockApi>, Arc<Mutex<MockClipState>>) {
    let api = MockApi::new();
    let (sys, st) = MockClipSys::new();
    let mut clip = Clipboard::new(Arc::clone(&api), sys);
    clip.copy_wait_secs = 0.05;
    (clip, api, st)
}

#[test]
fn original_unicode_image_file_and_registered_formats_restored() {
    let (clip, api, st) = make_clip();
    let mut guard = ForegroundGuard::new(api.as_ref()).unwrap();
    let st_copy = Arc::clone(&st);
    let exact = clip
        .copy_chat(
            42,
            &mut |_| {
                let mut s = st_copy.lock().unwrap();
                s.seq += 1;
                s.owner = 42;
                Ok(())
            },
            &mut guard,
            &mut || Ok(()),
        )
        .unwrap();
    assert_eq!(exact, "원문 🧪");
    let s = st.lock().unwrap();
    assert_eq!(s.restored.get(&13), Some(&513));
    assert_eq!(s.restored.get(&8), Some(&508));
    assert_eq!(s.restored.get(&15), Some(&515));
    assert_eq!(s.restored.get(&0xC117), Some(&(0xC117 + 500)));
    assert_eq!(s.restored_order, vec![13, 8, 15, 0xC117]);
    assert_eq!(s.empty_calls, 1);
    assert_eq!(s.free_calls, 0);
    assert_eq!(s.destroy_calls, 1);
}

#[test]
fn restore_runs_on_copy_and_decode_failures() {
    let (clip, api, st) = make_clip();
    st.lock().unwrap().unicode_bytes = vec![0x00, 0xD8, 0x00, 0x00];
    let mut guard = ForegroundGuard::new(api.as_ref()).unwrap();
    let st_copy = Arc::clone(&st);
    let err = clip
        .copy_chat(
            42,
            &mut |_| {
                let mut s = st_copy.lock().unwrap();
                s.seq += 1;
                s.owner = 42;
                Ok(())
            },
            &mut guard,
            &mut || Ok(()),
        )
        .unwrap_err();
    assert_eq!(err.message(), "clipboard_unicode_text_invalid");
    assert!(st.lock().unwrap().restored.contains_key(&8));
}

#[test]
fn unchanged_sequence_never_reads_prior_clipboard_as_messages() {
    let (clip, api, st) = make_clip();
    let mut guard = ForegroundGuard::new(api.as_ref()).unwrap();
    let err = clip
        .copy_chat(42, &mut |_| Ok(()), &mut guard, &mut || Ok(()))
        .unwrap_err();
    assert_eq!(err.message(), "clipboard_copy_not_observed");
    let s = st.lock().unwrap();
    assert_eq!(s.get_data_calls, 0);
    assert_eq!(s.empty_calls, 0);
}

#[test]
fn changed_external_clipboard_is_preserved() {
    let (clip, api, st) = make_clip();
    let mut guard = ForegroundGuard::new(api.as_ref()).unwrap();
    let st_copy = Arc::clone(&st);
    let err = clip
        .copy_chat(
            42,
            &mut |_| {
                let mut s = st_copy.lock().unwrap();
                s.seq += 1;
                s.owner = 55;
                Ok(())
            },
            &mut guard,
            &mut || Ok(()),
        )
        .unwrap_err();
    assert_eq!(err.message(), "clipboard_changed");
    assert_eq!(st.lock().unwrap().empty_calls, 0);
}

#[test]
fn opaque_format_stops_before_keys_are_sent() {
    let (clip, api, st) = make_clip();
    st.lock().unwrap().opaque_error = true;
    let mut guard = ForegroundGuard::new(api.as_ref()).unwrap();
    let mut performed = false;
    let err = clip
        .copy_chat(
            42,
            &mut |_| {
                performed = true;
                Ok(())
            },
            &mut guard,
            &mut || Ok(()),
        )
        .unwrap_err();
    assert_eq!(err.message(), "clipboard_format_cannot_be_preserved");
    assert!(!performed);
    assert_eq!(st.lock().unwrap().empty_calls, 0);
}

#[test]
fn newer_sequence_of_same_owner_is_not_erased() {
    let (clip, api, st) = make_clip();
    let mut guard = ForegroundGuard::new(api.as_ref()).unwrap();
    let st_copy = Arc::clone(&st);
    let st_check = Arc::clone(&st);
    let err = clip
        .copy_chat(
            42,
            &mut |_| {
                let mut s = st_copy.lock().unwrap();
                s.seq += 1;
                s.owner = 42;
                Ok(())
            },
            &mut guard,
            &mut || {
                st_check.lock().unwrap().seq += 1;
                Ok(())
            },
        )
        .unwrap_err();
    assert_eq!(err.message(), "clipboard_changed");
    assert_eq!(st.lock().unwrap().empty_calls, 0);
}

#[test]
fn concurrent_user_input_prevents_attributing_a_same_app_copy() {
    let (clip, api, st) = make_clip();
    let mut guard = ForegroundGuard::new(api.as_ref()).unwrap();
    let st_copy = Arc::clone(&st);
    let api_copy = Arc::clone(&api);
    let err = clip
        .copy_chat(
            42,
            &mut |_| {
                let mut s = st_copy.lock().unwrap();
                s.seq += 1;
                s.owner = 42;
                api_copy.st.lock().unwrap().last_input += 1;
                Ok(())
            },
            &mut guard,
            &mut || Ok(()),
        )
        .unwrap_err();
    assert_eq!(err.message(), "user_input_active");
    assert_eq!(st.lock().unwrap().empty_calls, 0);
}

#[test]
fn foreground_restores_window_child_and_minimized_state_on_drop() {
    let api = MockApi::new();
    api.st.lock().unwrap().minimized.insert(50);
    let err = (|| -> Result<()> {
        let guard = ForegroundGuard::activate_new(api.as_ref(), 50)?;
        guard.focus(501)?;
        Err(KaxError::blocked("simulated_operation_failure"))
    })()
    .unwrap_err();
    assert_eq!(err.message(), "simulated_operation_failure");
    let st = api.st.lock().unwrap();
    assert_eq!(st.fg, 31);
    assert_eq!(st.focuses.get(&31), Some(&310));
    assert_eq!(st.focuses.get(&50), Some(&500));
    assert!(st.show_calls.contains(&(50, 9)));
    assert!(st.show_calls.contains(&(50, 7)));
}

#[test]
fn user_input_prevents_keyboard_injection_and_focus_stealing() {
    let api = MockApi::new();
    let mut guard = ForegroundGuard::activate_new(api.as_ref(), 50).unwrap();
    {
        let mut st = api.st.lock().unwrap();
        st.last_input += 1;
        st.fg = 77;
    }
    let err = guard.keys(500, &[65, 67], true).unwrap_err();
    assert_eq!(err.message(), "user_input_active");
    guard.restore().unwrap();
    let st = api.st.lock().unwrap();
    assert_eq!(st.fg, 77);
    assert_eq!(st.input_keys_calls, 0);
}

#[test]
fn inactive_thread_focus_is_captured_after_activation_and_restored() {
    let api = MockApi::new();
    api.st.lock().unwrap().inactive_focus_zero = true;
    {
        let guard = ForegroundGuard::activate_new(api.as_ref(), 50).unwrap();
        assert_eq!(guard.target_focus, Some(500));
        guard.focus(501).unwrap();
        assert_eq!(api.st.lock().unwrap().focuses.get(&50), Some(&501));
    }
    let st = api.st.lock().unwrap();
    assert_eq!(st.fg, 31);
    assert_eq!(st.focuses.get(&50), Some(&500));
}

#[test]
fn foreground_refusal_reports_foreground_unavailable() {
    let api = MockApi::new();
    api.st.lock().unwrap().block_activate = true;
    let err = ForegroundGuard::activate_new(api.as_ref(), 50)
        .err()
        .expect("must fail");
    assert_eq!(err.message(), "foreground_unavailable");
}

#[test]
fn background_process_unlocks_foreground_and_restores_original() {
    let api = MockApi::new();
    {
        let guard = ForegroundGuard::activate_new(api.as_ref(), 50).unwrap();
        assert_eq!(api.st.lock().unwrap().fg, 50);
        guard.check().unwrap();
    }
    assert_eq!(api.st.lock().unwrap().fg, 31);
}

#[test]
fn null_foreground_fallback_tracks_unlock_stamp_without_false_user_input() {
    let api = MockApi::new();
    api.st.lock().unwrap().fg = 0;
    let guard = ForegroundGuard::activate_new(api.as_ref(), 50).unwrap();
    assert_eq!(api.st.lock().unwrap().fg, 50);
    guard.check().unwrap();
}

#[test]
fn own_send_input_tick_allowed_while_external_tick_blocks() {
    let api = MockApi::new();
    let mut guard = ForegroundGuard::activate_new(api.as_ref(), 50).unwrap();
    let stamp = guard.input_stamp().unwrap();
    api.st.lock().unwrap().last_input = stamp;
    guard.check().unwrap();

    api.st.lock().unwrap().last_input = stamp + 1;
    assert_eq!(guard.check().unwrap_err().message(), "user_input_active");
}

#[test]
fn arm_list_posts_background_gutter_click_when_tall_enough() {
    use kakaotalk_ax::win32_api::{arm_list_click_target, arm_list_with_ops};
    assert_eq!(arm_list_click_target(420), Some((4, 412)));
    assert_eq!(arm_list_click_target(16), Some((4, 8)));
    assert_eq!(arm_list_click_target(15), None);
    assert_eq!(arm_list_click_target(12), None);

    let mut clicks = Vec::new();
    arm_list_with_ops(201, 420, |hwnd, x, y| {
        clicks.push((hwnd, x, y));
        Ok(())
    })
    .unwrap();
    assert_eq!(clicks, vec![(201, 4, 412)]);

    let mut short_clicks = Vec::new();
    arm_list_with_ops(201, 12, |hwnd, x, y| {
        short_clicks.push((hwnd, x, y));
        Ok(())
    })
    .unwrap();
    assert!(short_clicks.is_empty());
}

#[test]
fn own_send_input_tick_does_not_self_block_idle_while_external_tick_blocks() {
    use std::collections::VecDeque;
    use kakaotalk_ax::win32_api::{is_idle_tick, send_input_with_ops};

    let own_ticks = Mutex::new(VecDeque::new());

    // Rejected SendInput must NOT record its tick into own_ticks.
    let err = send_input_with_ops(
        &[InputEvent::Key {
            vk: 0x41,
            flags: 0,
            timestamp: 900,
        }],
        &own_ticks,
        |_| Err(KaxError::blocked("keyboard_input_rejected")),
    )
    .unwrap_err();
    assert_eq!(err.message(), "keyboard_input_rejected");
    assert!(own_ticks.lock().unwrap().is_empty());

    // Successful SendInput records its event timestamp via production send_input_with_ops.
    send_input_with_ops(
        &[InputEvent::Key {
            vk: 0x41,
            flags: 0,
            timestamp: 1000,
        }],
        &own_ticks,
        |evs| {
            assert_eq!(evs.len(), 1);
            Ok(())
        },
    )
    .unwrap();
    let recorded: Vec<u32> = own_ticks.lock().unwrap().iter().copied().collect();
    assert_eq!(recorded, vec![1000]);

    // Own injected send_input tick passes idle check even when only 10ms elapsed (< 450ms threshold).
    assert!(is_idle_tick(1010, 1000, 450, &recorded, false));
    // External input tick within 450ms is blocked.
    assert!(!is_idle_tick(1010, 1005, 450, &recorded, false));
    // Any modifier key held blocks even if last_input is our own tick.
    assert!(!is_idle_tick(1010, 1000, 450, &recorded, true));
    // External input older than 450ms is idle.
    assert!(is_idle_tick(1500, 1000, 450, &[], false));

    // Repeated identical tick is deduplicated; 33rd unique tick evicts the oldest (cap = 32 unique).
    for _ in 0..5 {
        send_input_with_ops(
            &[InputEvent::Key { vk: 0x41, flags: 0, timestamp: 1000 }],
            &own_ticks,
            |_| Ok(()),
        )
        .unwrap();
    }
    assert_eq!(own_ticks.lock().unwrap().len(), 1);
    for t in 1001..=1032 {
        send_input_with_ops(
            &[InputEvent::Key { vk: 0x41, flags: 0, timestamp: t }],
            &own_ticks,
            |_| Ok(()),
        )
        .unwrap();
    }
    let after_33: Vec<u32> = own_ticks.lock().unwrap().iter().copied().collect();
    assert_eq!(after_33.len(), 32);
    assert!(!after_33.contains(&1000));
    assert_eq!(after_33.first(), Some(&1001));
    assert_eq!(after_33.last(), Some(&1032));
}

struct MockUnlockOps {
    fg: Mutex<usize>,
    threads: HashMap<usize, u32>,
    current_tid: u32,
    attached: Mutex<Vec<(u32, u32, bool)>>,
    unlock_stamps: Mutex<Vec<u32>>,
    allow_set_fg_when_attached: bool,
    allow_set_fg_after_unlock: bool,
    unlocked: Mutex<bool>,
    last_input: Mutex<u32>,
    tick: u32,
}

impl kakaotalk_ax::win32_api::ForegroundUnlockOps for MockUnlockOps {
    fn is_window(&self, _hwnd: usize) -> bool { true }
    fn is_iconic(&self, _hwnd: usize) -> bool { false }
    fn get_foreground_window(&self) -> usize { *self.fg.lock().unwrap() }
    fn pump_messages(&self) {}
    fn current_thread_id(&self) -> u32 { self.current_tid }
    fn window_thread_id(&self, hwnd: usize) -> u32 { *self.threads.get(&hwnd).unwrap_or(&0) }
    fn attach_thread_input(&self, current: u32, target: u32, attach: bool) -> bool {
        self.attached.lock().unwrap().push((current, target, attach));
        true
    }
    fn bring_window_to_top(&self, _hwnd: usize) {}
    fn set_foreground_window(&self, hwnd: usize) {
        let attached_now = {
            let log = self.attached.lock().unwrap();
            let a = log.iter().filter(|&&(_, _, on)| on).count();
            let d = log.iter().filter(|&&(_, _, on)| !on).count();
            a > d
        };
        if (self.allow_set_fg_when_attached && attached_now)
            || (self.allow_set_fg_after_unlock && *self.unlocked.lock().unwrap())
        {
            *self.fg.lock().unwrap() = hwnd;
        }
    }
    fn any_modifier_down(&self) -> bool { false }
    fn last_input(&self) -> Result<u32> { Ok(*self.last_input.lock().unwrap()) }
    fn tick(&self) -> u32 { self.tick }
    fn send_unlock_key(&self, stamp: u32) -> Result<()> {
        self.unlock_stamps.lock().unwrap().push(stamp);
        *self.unlocked.lock().unwrap() = true;
        *self.last_input.lock().unwrap() = stamp;
        Ok(())
    }
}

#[test]
fn production_activate_foreground_attaches_and_detaches_threads_without_unlock_key() {
    use kakaotalk_ax::win32_api::activate_foreground_with_ops;
    let ops = MockUnlockOps {
        fg: Mutex::new(31),
        threads: HashMap::from([(31, 200), (50, 300)]),
        current_tid: 100,
        attached: Mutex::new(Vec::new()),
        unlock_stamps: Mutex::new(Vec::new()),
        allow_set_fg_when_attached: true,
        allow_set_fg_after_unlock: false,
        unlocked: Mutex::new(false),
        last_input: Mutex::new(42),
        tick: 99,
    };
    let mut allowed = HashSet::from([42u32]);
    assert!(activate_foreground_with_ops(&ops, 50, Some(&mut allowed), 0.2));
    assert_eq!(*ops.fg.lock().unwrap(), 50);
    assert!(ops.unlock_stamps.lock().unwrap().is_empty());
    let log = ops.attached.lock().unwrap().clone();
    assert_eq!(
        log,
        vec![
            (100, 200, true),
            (100, 300, true),
            (100, 300, false),
            (100, 200, false),
        ]
    );
}

#[test]
fn production_activate_foreground_null_fg_tracks_unlock_stamp_and_uipi_refusal_returns_false() {
    use kakaotalk_ax::win32_api::activate_foreground_with_ops;
    // Null foreground -> thread attach alone fails -> falls back to VK_UNASSIGNED unlock key and tracks stamp
    let ops = MockUnlockOps {
        fg: Mutex::new(0),
        threads: HashMap::from([(50, 300)]),
        current_tid: 100,
        attached: Mutex::new(Vec::new()),
        unlock_stamps: Mutex::new(Vec::new()),
        allow_set_fg_when_attached: false,
        allow_set_fg_after_unlock: true,
        unlocked: Mutex::new(false),
        last_input: Mutex::new(42),
        tick: 777,
    };
    let mut allowed = HashSet::from([42u32]);
    assert!(activate_foreground_with_ops(&ops, 50, Some(&mut allowed), 0.1));
    assert_eq!(*ops.fg.lock().unwrap(), 50);
    assert_eq!(*ops.unlock_stamps.lock().unwrap(), vec![777]);
    assert!(allowed.contains(&777));

    // Elevated/UIPI refusal: neither attach nor unlock key changes foreground -> returns false
    let refused = MockUnlockOps {
        fg: Mutex::new(31),
        threads: HashMap::from([(31, 200), (50, 300)]),
        current_tid: 100,
        attached: Mutex::new(Vec::new()),
        unlock_stamps: Mutex::new(Vec::new()),
        allow_set_fg_when_attached: false,
        allow_set_fg_after_unlock: false,
        unlocked: Mutex::new(false),
        last_input: Mutex::new(42),
        tick: 888,
    };
    let mut allowed2 = HashSet::from([42u32]);
    assert!(!activate_foreground_with_ops(&refused, 50, Some(&mut allowed2), 0.05));
}
