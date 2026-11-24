use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use kakaotalk_ax::ax_core::KakaoBackend;
use kakaotalk_ax::error::{KaxError, Result};
use kakaotalk_ax::state::sha256_hex;
use kakaotalk_ax::win32_api::{DpiGuard, InputEvent, OcrBackend, Win32Api, Window, WM_CLOSE};
use kakaotalk_ax::win32_clipboard::{ClipboardBackend, ForegroundGuard};
use kakaotalk_ax::win32_engine::WindowsAx;
use kakaotalk_ax::win32_image::Frame;
use kakaotalk_ax::win32_rows::OcrLine;

const NAME: &str = "테스트 방 🧪";
const OTHER: &str = "다른 방";
const INITIAL: &str = "2026년 11월 22일 일요일\r\n[A] [오후 1:00] 기존 원문\r\n";

fn make_frame(
    width: usize,
    height: usize,
    rects: &[(usize, usize, usize, usize, (u8, u8, u8))],
) -> Frame {
    let mut data = Vec::with_capacity(width * height * 4);
    for _ in 0..(width * height) {
        data.extend_from_slice(&[220, 205, 185, 0]);
    }
    for &(left, top, right, bottom, (b, g, r)) in rects {
        for y in top..bottom {
            for x in left..right {
                let i = (y * width + x) * 4;
                data[i] = b;
                data[i + 1] = g;
                data[i + 2] = r;
                data[i + 3] = 0;
            }
        }
    }
    Frame::new(width, height, data)
}

fn yellow_ready_frame() -> Frame {
    make_frame(
        380,
        640,
        &[
            (0, 520, 380, 640, (255, 255, 255)),
            (296, 601, 370, 630, (0, 229, 254)),
            (311, 614, 333, 622, (30, 30, 30)),
            (347, 606, 348, 625, (0, 200, 220)),
            (356, 616, 361, 620, (30, 30, 30)),
        ],
    )
}

fn idle_split_frame() -> Frame {
    make_frame(
        380,
        640,
        &[
            (0, 520, 380, 640, (255, 255, 255)),
            (296, 601, 370, 630, (245, 245, 245)),
            (311, 614, 333, 622, (196, 196, 196)),
            (347, 606, 348, 625, (235, 235, 235)),
            (356, 616, 361, 620, (222, 222, 222)),
            (22, 538, 120, 550, (148, 148, 148)),
        ],
    )
}

struct DesktopState {
    root_windows: HashMap<usize, Window>,
    nodes: HashMap<usize, Vec<Window>>,
    hidden: HashSet<usize>,
    disabled: HashSet<usize>,
    minimized: HashSet<usize>,
    owners: HashMap<usize, usize>,
    contents: HashMap<usize, String>,
    transcripts: HashMap<usize, String>,
    clicked: Vec<usize>,
    right_clicks: Vec<usize>,
    closed: Vec<usize>,
    open_on_room_dblclick: Option<(usize, String)>,
    open_on_self_menu_click: Option<(usize, String)>,
    parents: HashMap<usize, usize>,
    delivered: bool,
    inactive: bool,
    input: u32,
    foreground: usize,
    focuses: HashMap<usize, usize>,
    send_ready: bool,
    custom_frame: Option<Frame>,
    selected_frame: Option<Frame>,
    ctrl_a_selected: bool,
    drag_selected: bool,
    drag_copied_text: Option<String>,
    arm_calls: Vec<usize>,
    key_batches: Vec<(usize, Vec<u16>, bool)>,
    clear_selection_calls: usize,
    gutter_clicks: Vec<(usize, i32, i32)>,
    armed: bool,
    require_physical_arm: bool,
    block_restore_to: Option<usize>,
}

struct Desktop {
    st: Mutex<DesktopState>,
}

impl Desktop {
    fn new() -> Arc<Self> {
        let mut root_windows = HashMap::new();
        root_windows.insert(
            10,
            Window {
                hwnd: 10,
                cls: "EVA_Window_Dblclk".into(),
                name: "카카오톡".into(),
                control_id: 0,
            },
        );
        let mut nodes = HashMap::new();
        nodes.insert(
            10,
            vec![
                Window {
                    hwnd: 100,
                    cls: "EVA_ChildWindow".into(),
                    name: "OnlineMainView_1".into(),
                    control_id: 132,
                },
                Window {
                    hwnd: 101,
                    cls: "EVA_ChildWindow_Dblclk".into(),
                    name: "LockModeView_1".into(),
                    control_id: 183,
                },
            ],
        );
        let mut hidden = HashSet::new();
        hidden.insert(101);
        let d = Arc::new(Self {
            st: Mutex::new(DesktopState {
                root_windows,
                nodes,
                hidden,
                disabled: HashSet::new(),
                minimized: HashSet::new(),
                owners: HashMap::new(),
                contents: HashMap::new(),
                transcripts: HashMap::new(),
                clicked: Vec::new(),
                right_clicks: Vec::new(),
                closed: Vec::new(),
                open_on_room_dblclick: None,
                open_on_self_menu_click: None,
                parents: HashMap::new(),
                delivered: true,
                inactive: false,
                input: 0,
                foreground: 10,
                focuses: HashMap::new(),
                send_ready: true,
                custom_frame: None,
                selected_frame: None,
                ctrl_a_selected: false,
                drag_selected: false,
                drag_copied_text: None,
                arm_calls: Vec::new(),
                key_batches: Vec::new(),
                clear_selection_calls: 0,
                gutter_clicks: Vec::new(),
                armed: false,
                require_physical_arm: false,
                block_restore_to: None,
            }),
        });
        d.add_chat(20, NAME);
        d.add_chat(30, OTHER);
        d
    }

    fn add_chat(&self, hwnd: usize, name: &str) {
        let mut st = self.st.lock().unwrap();
        st.root_windows.insert(
            hwnd,
            Window {
                hwnd,
                cls: "EVA_Window_Dblclk".into(),
                name: name.into(),
                control_id: 0,
            },
        );
        st.nodes.insert(
            hwnd,
            vec![
                Window {
                    hwnd: hwnd * 10,
                    cls: "RICHEDIT50W".into(),
                    name: String::new(),
                    control_id: 1006,
                },
                Window {
                    hwnd: hwnd * 10 + 1,
                    cls: "EVA_VH_ListControl_Dblclk".into(),
                    name: String::new(),
                    control_id: 100,
                },
                Window {
                    hwnd: hwnd * 10 + 2,
                    cls: "Edit".into(),
                    name: String::new(),
                    control_id: 100,
                },
            ],
        );
        st.contents.insert(hwnd * 10, String::new());
        st.transcripts.insert(hwnd, INITIAL.to_string());
    }

    fn enable_main_tabs(&self) {
        let mut st = self.st.lock().unwrap();
        st.nodes.insert(
            100,
            vec![
                Window {
                    hwnd: 1149,
                    cls: "EVA_ChildWindow".into(),
                    name: "ContactListView_1".into(),
                    control_id: 1149,
                },
                Window {
                    hwnd: 1150,
                    cls: "EVA_ChildWindow".into(),
                    name: "ChatRoomListView_1".into(),
                    control_id: 1150,
                },
            ],
        );
        st.parents.insert(1149, 100);
        st.parents.insert(1150, 100);
        st.nodes.insert(
            1149,
            vec![Window {
                hwnd: 11491,
                cls: "EVA_VH_ListControl_Dblclk".into(),
                name: "ContactListCtrl_1".into(),
                control_id: 1003,
            }],
        );
        st.parents.insert(11491, 1149);
        st.nodes.insert(
            1150,
            vec![
                Window {
                    hwnd: 11501,
                    cls: "Edit".into(),
                    name: String::new(),
                    control_id: 100,
                },
                Window {
                    hwnd: 11502,
                    cls: "EVA_VH_ListControl_Dblclk".into(),
                    name: "ChatRoomListCtrl_1".into(),
                    control_id: 1003,
                },
                Window {
                    hwnd: 11503,
                    cls: "EVA_VH_ListControl_Dblclk".into(),
                    name: "SearchListCtrl_1".into(),
                    control_id: 1003,
                },
            ],
        );
        st.parents.insert(11501, 1150);
        st.parents.insert(11502, 1150);
        st.parents.insert(11503, 1150);
    }
}

impl Win32Api for Desktop {
    fn handles(&self, parent: Option<usize>) -> Vec<usize> {
        let st = self.st.lock().unwrap();
        match parent {
            None => st.root_windows.keys().copied().collect(),
            Some(p) => st
                .nodes
                .get(&p)
                .map(|v| v.iter().map(|w| w.hwnd).collect())
                .unwrap_or_default(),
        }
    }
    fn title(&self, hwnd: usize) -> Result<String> {
        let st = self.st.lock().unwrap();
        if let Some(w) = st.root_windows.get(&hwnd) {
            return Ok(w.name.clone());
        }
        for list in st.nodes.values() {
            if let Some(w) = list.iter().find(|c| c.hwnd == hwnd) {
                return Ok(w.name.clone());
            }
        }
        Ok(String::new())
    }
    fn class_name(&self, hwnd: usize) -> String {
        let st = self.st.lock().unwrap();
        if let Some(w) = st.root_windows.get(&hwnd) {
            return w.cls.clone();
        }
        for list in st.nodes.values() {
            if let Some(w) = list.iter().find(|c| c.hwnd == hwnd) {
                return w.cls.clone();
            }
        }
        String::new()
    }
    fn dlg_ctrl_id(&self, hwnd: usize) -> i32 {
        let st = self.st.lock().unwrap();
        if let Some(w) = st.root_windows.get(&hwnd) {
            return w.control_id;
        }
        for list in st.nodes.values() {
            if let Some(w) = list.iter().find(|c| c.hwnd == hwnd) {
                return w.control_id;
            }
        }
        0
    }
    fn pid(&self, _hwnd: usize) -> u32 { 42 }
    fn process(&self, _pid: u32) -> (String, u64) { ("kakaotalk.exe".into(), 123) }
    fn is_window(&self, hwnd: usize) -> bool {
        let st = self.st.lock().unwrap();
        st.root_windows.contains_key(&hwnd)
            || st.nodes.values().any(|v| v.iter().any(|c| c.hwnd == hwnd))
    }
    fn is_window_visible(&self, hwnd: usize) -> bool { !self.st.lock().unwrap().hidden.contains(&hwnd) }
    fn is_window_enabled(&self, hwnd: usize) -> bool { !self.st.lock().unwrap().disabled.contains(&hwnd) }
    fn is_iconic(&self, hwnd: usize) -> bool { self.st.lock().unwrap().minimized.contains(&hwnd) }
    fn get_parent(&self, hwnd: usize) -> usize {
        if let Some(&p) = self.st.lock().unwrap().parents.get(&hwnd) {
            return p;
        }
        hwnd / 10
    }
    fn get_owner(&self, hwnd: usize) -> usize { *self.st.lock().unwrap().owners.get(&hwnd).unwrap_or(&0) }
    fn get_ancestor_root(&self, hwnd: usize) -> usize {
        if hwnd >= 200 { hwnd / 10 } else { hwnd }
    }
    fn get_dpi_for_window(&self, _hwnd: usize) -> u32 { 96 }
    fn get_foreground_window(&self) -> usize { self.st.lock().unwrap().foreground }
    fn set_foreground_window(&self, hwnd: usize) -> bool {
        let mut st = self.st.lock().unwrap();
        if st.block_restore_to == Some(hwnd) {
            return false;
        }
        st.foreground = hwnd;
        true
    }
    fn bring_window_to_top(&self, _hwnd: usize) -> bool { true }
    fn show_window_async(&self, hwnd: usize, cmd: i32) -> bool {
        let mut st = self.st.lock().unwrap();
        if cmd == 9 {
            st.minimized.remove(&hwnd);
        } else if cmd == 7 || cmd == 6 {
            st.minimized.insert(hwnd);
        }
        true
    }
    fn get_async_key_state(&self, _vk: i32) -> i16 { 0 }
    fn window_from_point(&self, _x: i32, _y: i32) -> usize {
        let st = self.st.lock().unwrap();
        if (st.require_physical_arm && !st.armed) || st.selected_frame.is_some() {
            st.foreground * 10 + 1
        } else {
            st.foreground
        }
    }
    fn control_text(&self, hwnd: usize) -> Result<String> {
        Ok(self.st.lock().unwrap().contents.get(&hwnd).cloned().unwrap_or_default())
    }
    fn set_text(&self, hwnd: usize, value: &str) -> Result<()> {
        self.st.lock().unwrap().contents.insert(hwnd, value.to_string());
        Ok(())
    }
    fn replace_edit(&self, hwnd: usize, value: &str) -> Result<()> {
        self.set_text(hwnd, value)
    }
    fn post(&self, hwnd: usize, code: u32, _wparam: usize, _lparam: isize) -> Result<()> {
        assert_eq!(code, WM_CLOSE);
        let mut st = self.st.lock().unwrap();
        st.closed.push(hwnd);
        st.root_windows.remove(&hwnd);
        Ok(())
    }
    fn click(&self, hwnd: usize, _x: i32, _y: i32, double: bool, right: bool) -> Result<()> {
        let mut add_new: Option<(usize, String)> = None;
        {
            let mut st = self.st.lock().unwrap();
            if right {
                st.right_clicks.push(hwnd);
                if hwnd == 11491 && st.open_on_self_menu_click.is_some() {
                    st.hidden.remove(&500);
                    st.root_windows.insert(
                        500,
                        Window {
                            hwnd: 500,
                            cls: "EVA_Menu".into(),
                            name: String::new(),
                            control_id: 0,
                        },
                    );
                    st.owners.insert(500, 10);
                }
                return Ok(());
            }
            if hwnd == 500 {
                st.root_windows.remove(&500);
                st.hidden.insert(500);
                if let Some((new_hwnd, title)) = st.open_on_self_menu_click.clone() {
                    st.foreground = new_hwnd;
                    add_new = Some((new_hwnd, title));
                }
            } else if (hwnd == 11502 || hwnd == 11503) && double {
                if let Some((new_hwnd, title)) = st.open_on_room_dblclick.clone() {
                    add_new = Some((new_hwnd, title));
                }
            } else {
                st.clicked.push(hwnd);
                if st.delivered {
                    let body = st.contents.get(&(hwnd * 10)).cloned().unwrap_or_default();
                    let entry = st.transcripts.entry(hwnd).or_default();
                    entry.push_str(&format!("[A] [오후 1:02] {body}\r\n"));
                    st.contents.insert(hwnd * 10, String::new());
                }
            }
        }
        if let Some((new_hwnd, title)) = add_new {
            self.add_chat(new_hwnd, &title);
        }
        Ok(())
    }
    fn wheel(&self, _hwnd: usize, _delta: i32) -> Result<()> { Ok(()) }
    fn tick(&self) -> u32 { self.st.lock().unwrap().input + 500 }
    fn last_input(&self) -> Result<u32> { Ok(self.st.lock().unwrap().input) }
    fn idle(&self, _ms: u32) -> Result<u32> {
        let st = self.st.lock().unwrap();
        if st.inactive {
            return Err(KaxError::blocked("user_input_active"));
        }
        Ok(st.input)
    }
    fn focus_hwnd(&self, hwnd: usize) -> Result<usize> {
        Ok(*self.st.lock().unwrap().focuses.get(&hwnd).unwrap_or(&0))
    }
    fn focus_child(&self, hwnd: usize) -> Result<()> {
        let mut st = self.st.lock().unwrap();
        let fg = st.foreground;
        st.focuses.insert(fg, hwnd);
        Ok(())
    }
    fn activate_foreground(&self, hwnd: usize, _t: Option<&mut HashSet<u32>>, _s: f64) -> bool {
        let mut st = self.st.lock().unwrap();
        if st.block_restore_to == Some(hwnd) {
            return false;
        }
        st.foreground = hwnd;
        true
    }
    fn pump_messages(&self) {}
    fn input_keys(&self, keys: &[(u16, bool)], _stamp: u32) -> Result<()> {
        let mut st = self.st.lock().unwrap();
        let fg = st.foreground;
        let child = *st.focuses.get(&fg).unwrap_or(&0);
        let control = keys.iter().any(|&(k, _)| k == 0x11);
        let main_keys: Vec<u16> = keys
            .iter()
            .filter(|&&(k, up)| k != 0x11 && !up)
            .map(|&(k, _)| k)
            .collect();
        st.key_batches.push((child, main_keys.clone(), control));
        if control && main_keys.contains(&65) && st.selected_frame.is_some() {
            st.ctrl_a_selected = true;
            st.drag_selected = false;
        }
        Ok(())
    }
    fn send_input(&self, _events: &[InputEvent]) -> Result<()> {
        let mut st = self.st.lock().unwrap();
        if st.selected_frame.is_some() {
            st.drag_selected = true;
            st.ctrl_a_selected = false;
        }
        Ok(())
    }
    fn cursor(&self) -> Result<(i32, i32)> { Ok((0, 0)) }
    fn mouse_click(&self, x: i32, y: i32, _stamp: u32) -> Result<()> {
        let mut st = self.st.lock().unwrap();
        let fg = st.foreground;
        if st.require_physical_arm && !st.armed {
            st.gutter_clicks.push((fg * 10 + 1, x, y));
            st.armed = true;
            return Ok(());
        }
        st.clicked.push(fg);
        if st.delivered {
            let body = st.contents.get(&(fg * 10)).cloned().unwrap_or_default();
            let entry = st.transcripts.entry(fg).or_default();
            entry.push_str(&format!("[A] [오후 1:02] {body}\r\n"));
            st.contents.insert(fg * 10, String::new());
        }
        Ok(())
    }
    fn mouse_move_event(&self, x: i32, y: i32, stamp: u32) -> Result<InputEvent> {
        Ok(InputEvent::Mouse { x, y, flags: 0, timestamp: stamp })
    }
    fn arm_list(&self, hwnd: usize) -> Result<()> {
        self.st.lock().unwrap().arm_calls.push(hwnd);
        Ok(())
    }
    fn clear_selection(&self, _hwnd: usize) -> Result<()> {
        let mut st = self.st.lock().unwrap();
        st.clear_selection_calls += 1;
        st.ctrl_a_selected = false;
        st.drag_selected = false;
        Ok(())
    }
    fn rect(&self, hwnd: usize, _client: bool) -> Result<(i32, i32, i32, i32)> {
        let st = self.st.lock().unwrap();
        if (hwnd >= 200 && hwnd % 10 == 1) || matches!(hwnd, 500 | 11491 | 11502 | 11503) {
            if let Some(ref f) = st.custom_frame {
                return Ok((0, 0, f.width as i32, f.height as i32));
            }
        }
        if hwnd >= 200 {
            Ok((14, 532, 379, 593))
        } else {
            Ok((0, 0, 380, 640))
        }
    }
    fn point(&self, _hwnd: usize, x: i32, y: i32, _to_screen: bool) -> Result<(i32, i32)> {
        Ok((x, y))
    }
    fn dpi_aware(&self) -> DpiGuard { DpiGuard::noop() }
    fn capture(&self, hwnd: usize) -> Result<Frame> {
        let st = self.st.lock().unwrap();
        if st.minimized.contains(&hwnd) {
            return Ok(make_frame(160, 28, &[]));
        }
        if st.ctrl_a_selected || st.drag_selected {
            if let Some(ref sf) = st.selected_frame {
                return Ok(sf.clone());
            }
        }
        if let Some(ref f) = st.custom_frame {
            return Ok(f.clone());
        }
        if st.send_ready {
            Ok(yellow_ready_frame())
        } else {
            Ok(idle_split_frame())
        }
    }
}

struct DesktopClipboard {
    api: Arc<Desktop>,
    copy_calls: Mutex<usize>,
}

impl ClipboardBackend<Desktop> for DesktopClipboard {
    fn copy_chat(
        &self,
        _pid: u32,
        perform: &mut dyn FnMut(&mut ForegroundGuard<'_, Desktop>) -> Result<()>,
        guard: &mut ForegroundGuard<'_, Desktop>,
        check_target: &mut dyn FnMut() -> Result<()>,
    ) -> Result<String> {
        *self.copy_calls.lock().unwrap() += 1;
        perform(guard)?;
        check_target()?;
        let st = self.api.st.lock().unwrap();
        if st.require_physical_arm && !st.armed {
            return Err(KaxError::blocked("clipboard_copy_not_observed"));
        }
        let fg = st.foreground;
        if st.drag_selected {
            if let Some(ref part) = st.drag_copied_text {
                return Ok(part.clone());
            }
        }
        Ok(st.transcripts.get(&fg).cloned().unwrap_or_default())
    }
}

struct MockOcr {
    lines: Mutex<Vec<OcrLine>>,
    calls: Mutex<usize>,
}

impl OcrBackend for MockOcr {
    fn check_available(&self, _language: &str) -> Result<()> { Ok(()) }
    fn recognize(&self, _language: &str, _frame: &Frame) -> Result<Vec<OcrLine>> {
        *self.calls.lock().unwrap() += 1;
        Ok(self.lines.lock().unwrap().clone())
    }
}

fn setup() -> (
    Arc<Desktop>,
    Arc<DesktopClipboard>,
    Arc<MockOcr>,
    WindowsAx<Desktop, DesktopClipboard, MockOcr>,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let api = Desktop::new();
    let clip = Arc::new(DesktopClipboard {
        api: Arc::clone(&api),
        copy_calls: Mutex::new(0),
    });
    let ocr = Arc::new(MockOcr {
        lines: Mutex::new(vec![OcrLine {
            text: "인식".into(),
            x: 0.0,
            y: 0.0,
            width: 6.0,
            height: 10.0,
        }]),
        calls: Mutex::new(0),
    });
    let win = WindowsAx::with_roots(
        Arc::clone(&api),
        Arc::clone(&clip),
        Arc::clone(&ocr),
        dir.path().join("ui"),
        dir.path().join("journal"),
    );
    (api, clip, ocr, win, dir)
}

#[test]
fn open_window_enumeration_flags_homonyms_without_reading() {
    let (api, clip, _ocr, win, _dir) = setup();
    api.add_chat(40, NAME);
    let wins = win.windows().unwrap();
    let readables: Vec<bool> = wins["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["windowTitle"] == NAME)
        .map(|c| c["readable"].as_bool().unwrap())
        .collect();
    assert_eq!(readables, vec![false, false]);
    assert_eq!(*clip.copy_calls.lock().unwrap(), 0);
    assert_eq!(
        win.open_room(NAME, false).unwrap()["reason"],
        "target_room_window_ambiguous"
    );
}

#[test]
fn send_target_is_exact_and_must_already_be_open() {
    let (api, _clip, _ocr, win, dir) = setup();
    for wrong in [format!("{NAME} "), "테스트 방".to_string(), "테스트 방  🧪".to_string()] {
        assert_eq!(
            win.send_message(&wrong, "본문", "exact", 0.0, false).unwrap()["reason"],
            "target_room_window_not_open"
        );
    }
    assert!(api.st.lock().unwrap().clicked.is_empty());
    let journal_dir = dir.path().join("journal");
    let json_count = std::fs::read_dir(&journal_dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("json"))
                .count()
        })
        .unwrap_or(0);
    assert_eq!(json_count, 0);
}

#[test]
fn duplicate_locked_login_and_owned_modal_are_blocked() {
    let (api, clip, _ocr, win, _dir) = setup();
    api.st.lock().unwrap().hidden.remove(&101);
    assert_eq!(
        win.send_message(NAME, "본문", "lock", 0.0, false).unwrap()["reason"],
        "lock_mode_active"
    );
    api.st.lock().unwrap().hidden.insert(101);

    let online = api.st.lock().unwrap().nodes.get_mut(&10).unwrap().remove(0);
    assert_eq!(
        win.send_message(NAME, "본문", "login", 0.0, false).unwrap()["reason"],
        "not_logged_in"
    );
    api.st.lock().unwrap().nodes.get_mut(&10).unwrap().push(online);

    {
        let mut st = api.st.lock().unwrap();
        st.root_windows.insert(
            40,
            Window {
                hwnd: 40,
                cls: "#32770".into(),
                name: "알림".into(),
                control_id: 0,
            },
        );
        st.owners.insert(40, 20);
    }
    assert_eq!(
        win.send_message(NAME, "본문", "modal", 0.0, false).unwrap()["reason"],
        "modal_dialog_present"
    );
    api.st.lock().unwrap().root_windows.remove(&40);

    api.add_chat(41, NAME);
    assert_eq!(
        win.send_message(NAME, "본문", "duplicate", 0.0, false).unwrap()["reason"],
        "target_room_window_ambiguous"
    );
    assert!(api.st.lock().unwrap().clicked.is_empty());
    assert_eq!(*clip.copy_calls.lock().unwrap(), 0);
}

#[test]
fn draft_one_character_is_preserved_without_copy_or_click() {
    let (api, clip, _ocr, win, _dir) = setup();
    api.st.lock().unwrap().contents.insert(200, " ".into());
    assert_eq!(
        win.close_room(NAME).unwrap()["reason"],
        "existing_draft_preserved"
    );
    assert_eq!(
        win.send_message(NAME, "새 본문", "draft", 0.0, false).unwrap()["reason"],
        "existing_draft_preserved"
    );
    assert_eq!(api.st.lock().unwrap().contents.get(&200).map(String::as_str), Some(" "));
    assert!(api.st.lock().unwrap().clicked.is_empty());
    assert!(api.st.lock().unwrap().closed.is_empty());
    assert_eq!(*clip.copy_calls.lock().unwrap(), 0);
}

#[test]
fn verify_self_reports_hidden_main_window_before_foreground_switch() {
    let (api, _clip, _ocr, win, _dir) = setup();
    {
        let mut st = api.st.lock().unwrap();
        st.foreground = 99;
        st.hidden.insert(10);
    }
    assert_eq!(
        win.open_room(NAME, true).unwrap()["reason"],
        "main_window_not_visible"
    );
}

#[test]
fn send_journal_is_written_before_dispatch_and_never_resends() {
    let (api, _clip, _ocr, win, dir) = setup();
    let body = "한글 🧪\n둘째 줄";
    let sent = win.send_message(NAME, body, "once", 0.0, false).unwrap();
    assert_eq!(sent["status"], "observed_in_ax");
    assert_eq!(sent["confirmationSource"], "windows_clipboard");

    let jpath = dir
        .path()
        .join("journal")
        .join(format!("{}.json", sha256_hex("once")));
    assert!(!std::fs::read_to_string(&jpath).unwrap().contains(body));

    let again = win.send_message(NAME, body, "once", 0.0, false).unwrap();
    assert_eq!(again["nativeSendCalls"], 0);
    assert_eq!(again["deduplicated"], true);
    assert_eq!(win.send_result("once", 0.0).unwrap()["status"], "observed_in_ax");
    assert_eq!(api.st.lock().unwrap().clicked, vec![20]);
}

#[test]
fn unknown_send_never_reopens_or_retries_after_handle_replacement() {
    let (api, _clip, _ocr, win, _dir) = setup();
    api.st.lock().unwrap().delivered = false;
    let initial = win
        .send_message(NAME, "기존 원문", "unsure", 0.0, false)
        .unwrap();
    assert_eq!(initial["status"], "outcome_unknown");

    api.post(20, WM_CLOSE, 0, 0).unwrap();
    api.add_chat(22, NAME);
    api.st
        .lock()
        .unwrap()
        .transcripts
        .get_mut(&22)
        .unwrap()
        .push_str("[A] [오후 1:02] 기존 원문\r\n");

    assert_eq!(win.send_result("unsure", 0.0).unwrap()["status"], "outcome_unknown");
    assert_eq!(
        win.send_message(NAME, "기존 원문", "unsure", 0.0, false).unwrap()["nativeSendCalls"],
        0
    );
    assert_eq!(api.st.lock().unwrap().clicked, vec![20]);
}

#[test]
fn disabled_button_does_not_dispatch_and_only_helper_draft_is_cleared() {
    let (api, _clip, _ocr, win, _dir) = setup();
    api.st.lock().unwrap().send_ready = false;
    let result = win.send_message(NAME, "도움말", "disabled", 0.0, false).unwrap();
    assert_eq!(result["status"], "blocked");
    assert_eq!(result["reason"], "send_button_not_enabled");
    assert_eq!(result["native"]["dispatchAttempted"], false);
    assert!(api.st.lock().unwrap().clicked.is_empty());
    assert_eq!(api.st.lock().unwrap().contents.get(&200).map(String::as_str), Some(""));
}

#[test]
fn new_input_blocks_read_and_send_before_dispatch() {
    let (api, _clip, _ocr, win, _dir) = setup();
    api.st.lock().unwrap().inactive = true;
    assert_eq!(
        win.read_messages(NAME, 50, 0, "").unwrap()["reason"],
        "user_input_active"
    );
    assert_eq!(
        win.send_message(NAME, "본문", "conflict", 0.0, false).unwrap()["reason"],
        "user_input_active"
    );
    assert!(api.st.lock().unwrap().clicked.is_empty());
}

#[test]
fn snapshot_change_detected_even_when_row_count_remains_equal() {
    let (api, _clip, _ocr, win, _dir) = setup();
    let snap = win.read_messages(NAME, 1, 0, "").unwrap()["snapshotId"]
        .as_str()
        .unwrap()
        .to_string();
    // Token issued by the Python backend for this exact transcript.
    assert_eq!(snap, "3759b649a23b2054bff11a89");
    api.st
        .lock()
        .unwrap()
        .transcripts
        .insert(20, INITIAL.replace("기존 원문", "수정 문자"));
    let page = win.read_messages(NAME, 1, 1, &snap).unwrap();
    assert_eq!(page["status"], "snapshot_changed");
    assert_eq!(page["items"], serde_json::json!([]));
    assert_ne!(page["snapshotId"], snap);
}

#[test]
fn serial_batch_reports_parallel_false_and_preserves_open_windows() {
    let (api, _clip, ocr, win, _dir) = setup();
    api.enable_main_tabs();
    // Pre-existing open window NAME (hwnd 20) + closed room "새 방" opened during batch (hwnd 40).
    api.st.lock().unwrap().open_on_room_dblclick = Some((40, "새 방".into()));
    api.st.lock().unwrap().custom_frame = Some(make_frame(
        340,
        72,
        &[
            (0, 0, 340, 72, (255, 255, 255)),
            (18, 12, 58, 52, (60, 120, 200)),
            (70, 16, 150, 32, (30, 30, 30)),
        ],
    ));
    *ocr.lines.lock().unwrap() = vec![OcrLine {
        text: "새 방".into(),
        x: 68.0,
        y: 12.0,
        width: 48.0,
        height: 16.0,
    }];
    let result = win
        .read_many(&[NAME.to_string(), "새 방".to_string()], 50, true, true, 4)
        .unwrap();
    assert_eq!(result["status"], "ok");
    assert_eq!(result["parallel"], false);
    assert_eq!(result["windowsOpened"], serde_json::json!(["새 방"]));
    // Only the newly opened window (40) is closed; pre-existing NAME (20) stays open.
    let st = api.st.lock().unwrap();
    assert_eq!(st.closed, vec![40]);
    assert!(st.root_windows.contains_key(&20));
}

#[test]
fn snapshot_arms_list_separates_ctrl_a_ctrl_c_and_falls_back_to_physical_gutter_click() {
    let (api, _clip, _ocr, win, _dir) = setup();
    {
        let mut st = api.st.lock().unwrap();
        st.require_physical_arm = true;
        st.custom_frame = Some(make_frame(
            380,
            419,
            &[(40, 40, 180, 90, (4, 222, 250))],
        ));
    }
    let chat = win.exact_chat(&win.app(true).unwrap(), NAME, false).unwrap().unwrap();
    let (_snap, rows, _) = win.snapshot(&chat, None).unwrap();
    assert_eq!(rows.len(), 2);
    let st = api.st.lock().unwrap();
    assert_eq!(st.arm_calls, vec![chat.history, chat.history]);
    assert_eq!(st.gutter_clicks, vec![(chat.history, 4, 411)]);
    assert_eq!(
        st.key_batches,
        vec![
            (chat.history, vec![65], true),
            (chat.history, vec![67], true),
            (chat.history, vec![65], true),
            (chat.history, vec![67], true),
        ]
    );
    assert!(st.clear_selection_calls >= 1);
}

#[test]
fn minimized_chat_composer_empty_restores_for_placeholder_and_preserves_real_draft() {
    let (api, _clip, _ocr, win, _dir) = setup();
    {
        let mut st = api.st.lock().unwrap();
        st.minimized.insert(20);
        st.contents.insert(200, "메시지 입력".into());
        st.custom_frame = Some(idle_split_frame());
    }
    let chat = win.exact_chat(&win.app(true).unwrap(), NAME, false).unwrap().unwrap();
    assert!(win.composer_empty(&chat).unwrap());
    assert!(api.st.lock().unwrap().minimized.contains(&20));

    // Armed yellow send button with "메시지 입력" text is preserved as a user draft!
    api.st.lock().unwrap().custom_frame = Some(yellow_ready_frame());
    assert!(!win.composer_empty(&chat).unwrap());
    assert_eq!(
        win.close_room(NAME).unwrap()["reason"],
        "existing_draft_preserved"
    );

    // Normal non-placeholder draft returns false immediately
    api.st.lock().unwrap().contents.insert(200, "작성 중인 초안".into());
    assert_eq!(
        win.send_message(NAME, "본문", "min-draft", 0.0, false).unwrap()["reason"],
        "existing_draft_preserved"
    );

    // Idle placeholder on minimized window closes cleanly
    {
        let mut st = api.st.lock().unwrap();
        st.contents.insert(200, "메시지 입력".into());
        st.custom_frame = Some(idle_split_frame());
    }
    assert_eq!(win.close_room(NAME).unwrap()["status"], "ok");
}

#[test]
fn require_self_does_not_bypass_evidence() {
    let (api, _clip, _ocr, win, _dir) = setup();
    api.enable_main_tabs();
    // ContactListCtrl_1 exists so verify_self actually executes right-click verification,
    // finds no EVA_Menu self-chat evidence, and blocks before any send click.
    let res = win.send_message(NAME, "새 본문", "not-self", 0.0, true).unwrap();
    assert_eq!(res["status"], "blocked");
    assert_eq!(res["reason"], "self_chat_unverified");
    assert_eq!(api.st.lock().unwrap().right_clicks, vec![11491]);
    assert!(api.st.lock().unwrap().clicked.is_empty());
}

#[test]
fn attachment_rechecks_snapshot_and_rejects_existing_output_path() {
    let (api, _clip, _ocr, win, dir) = setup();
    api.st
        .lock()
        .unwrap()
        .transcripts
        .get_mut(&20)
        .unwrap()
        .push_str("[A] [오후 1:01] 사진\r\n");
    let page = win.read_messages(NAME, 50, 0, "").unwrap();
    let snap = page["snapshotId"].as_str().unwrap().to_string();
    let row = page["items"].as_array().unwrap().last().unwrap()["rowIndex"]
        .as_i64()
        .unwrap();

    // Stale snapshot returns snapshot_changed
    let stale = win
        .read_attachment(NAME, row, &"0".repeat(24), None, false)
        .unwrap();
    assert_eq!(stale["status"], "snapshot_changed");

    // Non-attachment row blocks
    let non_att = win.read_attachment(NAME, 1, &snap, None, false).unwrap();
    assert_eq!(non_att["reason"], "attachment_row_required");

    // Existing output path rejected before UI access
    let existing = dir.path().join("exists.png");
    std::fs::write(&existing, b"already").unwrap();
    let err = win
        .read_attachment(NAME, row, &snap, Some(existing.to_str().unwrap()), false)
        .unwrap_err();
    assert!(err.message().contains("already exists"));

    // Success path: valid rendered photo box + darkened selection band
    let clean = make_frame(
        380,
        300,
        &[(50, 90, 210, 210, (30, 80, 190))],
    );
    let selected = make_frame(
        380,
        300,
        &[
            (8, 86, 370, 214, (140, 130, 118)),
            (50, 90, 210, 210, (18, 52, 125)),
        ],
    );
    {
        let mut st = api.st.lock().unwrap();
        st.custom_frame = Some(clean);
        st.selected_frame = Some(selected);
        st.drag_copied_text = Some("[A] [오후 1:01] 사진\r\n".into());
    }

    let out_no_ocr = dir.path().join("photo_no_ocr.png");
    let res_no_ocr = win
        .read_attachment(NAME, row, &snap, Some(out_no_ocr.to_str().unwrap()), false)
        .unwrap();
    assert_eq!(res_no_ocr["status"], "ok");
    assert_eq!(res_no_ocr["scrollRestored"], true);
    assert_eq!(res_no_ocr["ocrPerformed"], false);
    assert!(res_no_ocr.get("ocr").is_none());
    let png_bytes = std::fs::read(&out_no_ocr).unwrap();
    assert!(png_bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert_eq!(*_ocr.calls.lock().unwrap(), 0);

    let out_with_ocr = dir.path().join("photo_with_ocr.png");
    let res_with_ocr = win
        .read_attachment(NAME, row, &snap, Some(out_with_ocr.to_str().unwrap()), true)
        .unwrap();
    assert_eq!(res_with_ocr["status"], "ok");
    assert_eq!(res_with_ocr["scrollRestored"], true);
    assert_eq!(res_with_ocr["ocrPerformed"], true);
    assert_eq!(res_with_ocr["ocr"], "인식");
    assert!(std::fs::read(&out_with_ocr)
        .unwrap()
        .starts_with(b"\x89PNG\r\n\x1a\n"));
    assert_eq!(*_ocr.calls.lock().unwrap(), 1);
}

#[test]
fn self_alias_detection_and_send_exact_title_guard() {
    use kakaotalk_ax::win32_engine::is_self_alias;
    for alias in ["나", "나와의 채팅", "Chat with myself", "me"] {
        assert!(is_self_alias(alias), "expected alias: {alias}");
    }
    assert!(!is_self_alias(OTHER));

    let (api, _clip, ocr, win, _dir) = setup();
    api.enable_main_tabs();
    api.st.lock().unwrap().open_on_self_menu_click = Some((50, "이현섭".into()));
    api.st.lock().unwrap().custom_frame = Some(make_frame(
        240,
        120,
        &[(10, 10, 100, 30, (20, 20, 20))],
    ));
    *ocr.lines.lock().unwrap() = vec![
        OcrLine {
            text: "나와의 채팅".into(),
            x: 12.0,
            y: 12.0,
            width: 80.0,
            height: 16.0,
        },
        OcrLine {
            text: "프로필 보기".into(),
            x: 12.0,
            y: 44.0,
            width: 80.0,
            height: 16.0,
        },
        OcrLine {
            text: "프로필 편집".into(),
            x: 12.0,
            y: 76.0,
            width: 80.0,
            height: 16.0,
        },
    ];

    // With require_self=true, self-alias "나와의 채팅" opens verified self chat "이현섭".
    let opened = win.open_room("나와의 채팅", true).unwrap();
    assert_eq!(opened["status"], "ok");
    assert_eq!(opened["windowTitle"], "이현섭");
    assert_eq!(opened["isSelf"], true);

    // Without require_self, "나" is NOT treated as a self-alias (searches literal room "나" and blocks).
    let non_self = win.open_room("나", false).unwrap();
    assert_eq!(non_self["status"], "blocked");
    assert_eq!(non_self["reason"], "room_not_in_exposed_list");

    // If a friend room literally titled "나" is ALREADY OPEN, open_room("나", require_self=true)
    // must NOT treat it as the verified self-chat ("이현섭") and must reject with self_chat_unverified.
    api.add_chat(60, "나");
    let homonym_self = win.open_room("나", true).unwrap();
    assert_eq!(homonym_self["status"], "blocked");
    assert_eq!(homonym_self["reason"], "self_chat_unverified");

    // Sending to self-alias "나" when no such room is open (or when it is not verified self) is rejected.
    api.st.lock().unwrap().root_windows.remove(&60);
    assert_eq!(
        win.send_message("나", "본문", "alias-blocked", 0.0, true).unwrap()["reason"],
        "target_room_window_not_open"
    );
}

#[test]
fn send_never_accepts_self_aliases() {
    let (_api, _clip, _ocr, win, _dir) = setup();
    assert_eq!(
        win.send_message("나", "본문", "alias-blocked", 0.0, true).unwrap()["reason"],
        "target_room_window_not_open"
    );
}

#[test]
fn normal_room_split_send_button_idle_and_ready_detection() {
    let (api, _clip, _ocr, win, _dir) = setup();
    let chat = win.exact_chat(&win.app(true).unwrap(), OTHER, false).unwrap().unwrap();
    {
        let mut st = api.st.lock().unwrap();
        st.contents.insert(chat.composer, "메시지 입력".into());
        st.custom_frame = Some(idle_split_frame());
    }
    assert!(win.composer_empty(&chat).unwrap());
    assert!(!win.send_button_ready(&chat).unwrap());

    api.st.lock().unwrap().custom_frame = Some(yellow_ready_frame());
    assert!(!win.composer_empty(&chat).unwrap());
    assert!(win.send_button_ready(&chat).unwrap());
}

#[test]
fn foreground_restore_failure_is_propagated_not_swallowed_in_drop() {
    let (api, _clip, _ocr, win, _dir) = setup();
    // Initial foreground is 10; activating chat 20 succeeds, restoring back to 10 fails.
    api.st.lock().unwrap().block_restore_to = Some(10);
    let res = win.read_messages(NAME, 50, 0, "").unwrap();
    assert_eq!(res["status"], "blocked");
    assert_eq!(res["reason"], "foreground_restore_failed");
}
