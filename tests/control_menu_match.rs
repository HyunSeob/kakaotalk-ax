//! Synthetic AX tree decision tests corresponding to `tests/test_control_menu_match.py`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use kakaotalk_ax::error::Result;
use kakaotalk_ax::macos_ax_logic::{
    check_search_toolbar_geometry, decide_room_open, room_entries,
    select_open_menu_item, validate_search_toolbar_buttons, AxFrame, AxTree, AxValue, RoomEntry,
    RoomLookupDecision,
};

#[derive(Default)]
struct SyntheticNode {
    attrs: HashMap<String, AxValue>,
}

struct SyntheticTree {
    nodes: Vec<SyntheticNode>,
    windows: Vec<usize>,
    actions: HashMap<usize, Vec<String>>,
    metadata_allowed: AtomicBool,
    metadata_reads: AtomicUsize,
    name_id: Option<usize>,
    table_id: Option<usize>,
    frames: HashMap<usize, AxFrame>,
}

impl SyntheticTree {
    fn new() -> Self {
        Self {
            nodes: Vec::new(),
            windows: Vec::new(),
            actions: HashMap::new(),
            metadata_allowed: AtomicBool::new(true),
            metadata_reads: AtomicUsize::new(0),
            name_id: None,
            table_id: None,
            frames: HashMap::new(),
        }
    }

    fn add(&mut self, role: &str) -> usize {
        let id = self.nodes.len();
        let mut attrs = HashMap::new();
        attrs.insert("AXRole".to_string(), AxValue::String(role.to_string()));
        self.nodes.push(SyntheticNode { attrs });
        id
    }

    fn set(&mut self, id: usize, key: &str, val: AxValue) {
        self.nodes[id].attrs.insert(key.to_string(), val);
    }
}

impl AxTree for SyntheticTree {
    fn app_windows(&self) -> Result<Vec<usize>> {
        Ok(self.windows.clone())
    }

    fn attr(&self, id: usize, name: &str) -> Result<Option<AxValue>> {
        if !self.metadata_allowed.load(Ordering::SeqCst) {
            let allowed_key = name == "AXRole"
                || name == "AXChildren"
                || (self.table_id == Some(id) && name == "AXVisibleRows")
                || (self.name_id == Some(id) && name == "AXValue");
            if !allowed_key {
                self.metadata_reads.fetch_add(1, Ordering::SeqCst);
                panic!("unexpected metadata attribute {name} on node {id}");
            }
        }
        Ok(self.nodes[id].attrs.get(name).cloned())
    }

    fn attr_count(&self, id: usize, name: &str) -> Result<Option<usize>> {
        if self.table_id == Some(id) && name == "AXRows" {
            if let Some(AxValue::Elements(v)) = self.nodes[id].attrs.get("AXRows") {
                return Ok(Some(v.len()));
            }
        }
        Ok(Some(self.elements_attr(id, name)?.len()))
    }

    fn action_names(&self, id: usize) -> Vec<String> {
        self.actions.get(&id).cloned().unwrap_or_default()
    }

    fn frame(&self, id: usize) -> Result<Option<AxFrame>> {
        Ok(self.frames.get(&id).copied())
    }
}

#[test]
fn menu_filter_and_uniqueness_guard() {
    let make_case = |items: &[(&str, &str, Option<AxValue>, &str)]| -> (SyntheticTree, Vec<usize>) {
        let mut tree = SyntheticTree::new();
        let menu = tree.add("AXMenu");
        let mut children = Vec::new();
        for &(title, ident, ref enabled, role) in items {
            let el = tree.add(role);
            tree.set(el, "AXTitle", AxValue::String(title.to_string()));
            tree.set(el, "AXIdentifier", AxValue::String(ident.to_string()));
            if let Some(en) = enabled {
                tree.set(el, "AXEnabled", en.clone());
            }
            children.push(el);
        }
        tree.set(menu, "AXChildren", AxValue::Elements(children));
        (tree, vec![menu])
    };

    // Valid localized / English / Korean / empty titles with openChatRoom: + AXEnabled=true
    for title in ["Localized alternative", "Open Chat", "채팅방 열기", ""] {
        let (tree, menus) = make_case(&[(title, "openChatRoom:", Some(AxValue::Bool(true)), "AXMenuItem")]);
        let selected = select_open_menu_item(&tree, &menus).unwrap();
        assert_eq!(tree.string_attr(selected, "AXTitle", true).unwrap(), title);
    }

    // Invalid cases must all block with open_menu_item_not_unique
    let invalid_cases: Vec<Vec<(&str, &str, Option<AxValue>, &str)>> = vec![
        vec![("Open Chat", "other:", Some(AxValue::Bool(true)), "AXMenuItem")],
        vec![("Open Chat", "openChatRoom:", Some(AxValue::Bool(false)), "AXMenuItem")],
        vec![("Open Chat", "openChatRoom:", Some(AxValue::Bool(true)), "AXButton")],
        vec![("Open Chat", "openChatRoom:", None, "AXMenuItem")],
        vec![("Open Chat", "openChatRoom:", Some(AxValue::String("true".into())), "AXMenuItem")],
        vec![
            ("One", "openChatRoom:", Some(AxValue::Bool(true)), "AXMenuItem"),
            ("Two", "openChatRoom:", Some(AxValue::Bool(true)), "AXMenuItem"),
        ],
        vec![],
    ];
    for case in invalid_cases {
        let (tree, menus) = make_case(&case);
        let err = select_open_menu_item(&tree, &menus).unwrap_err();
        assert_eq!(err.message(), "open_menu_item_not_unique");
    }

    // Empty menus list
    let empty_tree = SyntheticTree::new();
    assert_eq!(
        select_open_menu_item(&empty_tree, &[]).unwrap_err().message(),
        "open_menu_item_not_unique"
    );

    // Disabled + enabled -> picks the unique enabled one
    let (tree, menus) = make_case(&[
        ("Disabled", "openChatRoom:", Some(AxValue::Bool(false)), "AXMenuItem"),
        ("Open Chat", "openChatRoom:", Some(AxValue::Bool(true)), "AXMenuItem"),
    ]);
    let selected = select_open_menu_item(&tree, &menus).unwrap();
    assert_eq!(tree.string_attr(selected, "AXTitle", true).unwrap(), "Open Chat");
}

#[test]
fn room_lookup_preserves_metadata_and_require_self() {
    let mut tree = SyntheticTree::new();
    let name = tree.add("AXStaticText");
    tree.set(name, "AXValue", AxValue::String("Fixture group".into()));
    let time = tree.add("AXStaticText");
    tree.set(time, "AXValue", AxValue::String("12:34".into()));
    let preview = tree.add("AXTextArea");
    tree.set(preview, "AXValue", AxValue::String("Synthetic preview".into()));
    let badge = tree.add("AXImage");
    tree.set(badge, "AXDescription", AxValue::String("badge me".into()));
    let cell = tree.add("AXCell");
    tree.set(cell, "AXChildren", AxValue::Elements(vec![name, time, preview, badge]));
    let row = tree.add("AXRow");
    tree.set(row, "AXChildren", AxValue::Elements(vec![cell]));
    tree.set(row, "AXIndex", AxValue::Int(7));
    let table = tree.add("AXTable");
    tree.set(table, "AXVisibleRows", AxValue::Elements(vec![row]));
    tree.set(table, "AXRows", AxValue::Elements(vec![row]));
    let main = tree.add("AXWindow");
    tree.set(main, "AXChildren", AxValue::Elements(vec![table]));

    tree.name_id = Some(name);
    tree.table_id = Some(table);

    // Ordinary open (names_only = true) must not read preview, time, row index, or self badge.
    tree.metadata_allowed.store(false, Ordering::SeqCst);
    let (entries, _, _) = room_entries(&tree, main, true).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "Fixture group");
    assert_eq!(entries[0].cell, cell);
    assert_eq!(tree.metadata_reads.load(Ordering::SeqCst), 0);

    // Empty name skipped
    tree.set(name, "AXValue", AxValue::String(String::new()));
    assert!(room_entries(&tree, main, true).unwrap().0.is_empty());

    // Missing label skipped
    tree.set(name, "AXValue", AxValue::String("Fixture group".into()));
    tree.set(cell, "AXChildren", AxValue::Elements(vec![preview]));
    assert!(room_entries(&tree, main, true).unwrap().0.is_empty());

    // Boundary 1000 rows OK, 1001 rows blocked with room_list_limit
    tree.set(cell, "AXChildren", AxValue::Elements(vec![name, time, preview, badge]));
    tree.set(table, "AXVisibleRows", AxValue::Elements(vec![]));
    assert!(room_entries(&tree, main, true).unwrap().0.is_empty());

    tree.set(table, "AXVisibleRows", AxValue::Elements(vec![row; 1000]));
    assert_eq!(room_entries(&tree, main, true).unwrap().0.len(), 1000);

    tree.set(table, "AXVisibleRows", AxValue::Elements(vec![row; 1001]));
    assert_eq!(
        room_entries(&tree, main, true).unwrap_err().message(),
        "room_list_limit"
    );

    // Full metadata read (rooms / search_rooms / require_self=true)
    tree.set(table, "AXVisibleRows", AxValue::Elements(vec![row]));
    tree.metadata_allowed.store(true, Ordering::SeqCst);
    let (full, reported, source) = room_entries(&tree, main, false).unwrap();
    assert_eq!(reported, 1);
    assert_eq!(source, "AXVisibleRows");
    assert_eq!(full[0].name, "Fixture group");
    assert!(full[0].is_self);
    assert_eq!(full[0].row_index, 7);
    assert_eq!(full[0].labels, vec!["Fixture group", "12:34"]);
    assert_eq!(full[0].preview.as_deref(), Some("Synthetic preview"));
    assert_eq!(
        decide_room_open(&tree, &full, "Fixture group", true).unwrap(),
        RoomLookupDecision::OpenCell(cell)
    );

    // Unverified self badge blocks
    tree.set(badge, "AXDescription", AxValue::String("other badge".into()));
    let (unverified, _, _) = room_entries(&tree, main, false).unwrap();
    assert_eq!(
        decide_room_open(&tree, &unverified, "Fixture group", true)
            .unwrap_err()
            .message(),
        "self_room_not_verified"
    );
}

#[test]
fn exact_room_guards_and_already_open_path() {
    let run = |names: &[&str], already_open: bool, require_self: bool, title: &str| {
        let mut tree = SyntheticTree::new();
        if already_open {
            let w = tree.add("AXWindow");
            tree.set(w, "AXTitle", AxValue::String("Exact fixture".into()));
            tree.windows.push(w);
        }
        let entries: Vec<RoomEntry> = names
            .iter()
            .enumerate()
            .map(|(i, n)| RoomEntry {
                cell: i,
                name: (*n).to_string(),
                is_self: false,
                row_index: i as i64,
                labels: vec![],
                preview: None,
            })
            .collect();
        decide_room_open(&tree, &entries, title, require_self)
    };

    assert!(matches!(run(&["Exact fixture"], false, false, "Exact fixture"), Ok(RoomLookupDecision::OpenCell(_))));
    assert_eq!(run(&["Exact fixture", "Exact fixture"], false, false, "Exact fixture").unwrap_err().message(), "target_room_name_ambiguous");
    assert_eq!(run(&[], false, false, "Exact fixture").unwrap_err().message(), "room_not_in_exposed_list");
    assert_eq!(run(&["Exact fixture suffix"], false, false, "Exact fixture").unwrap_err().message(), "room_not_in_exposed_list");
    assert_eq!(run(&["Other"], false, false, "Exact fixture").unwrap_err().message(), "room_not_in_exposed_list");
    assert_eq!(run(&[], true, false, "Exact fixture").unwrap(), RoomLookupDecision::AlreadyOpen);
    assert_eq!(run(&["Exact fixture", "Exact fixture"], true, false, "Exact fixture").unwrap_err().message(), "target_room_name_ambiguous");
    assert_eq!(run(&[], true, true, "Exact fixture").unwrap_err().message(), "room_not_in_exposed_list");
    assert_eq!(run(&["Exact fixture"], true, true, "Exact fixture").unwrap_err().message(), "self_room_not_verified");
    assert_eq!(run(&["Exact fixture"], false, false, "").unwrap_err().message(), "exact_window_title_required");
}

fn frame(x: f64, y: f64, w: f64, h: f64) -> AxFrame {
    AxFrame { x, y, w, h }
}

/// Search field + 5 toolbar buttons as measured on KakaoTalk Mac 26.10.0
/// (field 221x22 at (1235,389); 28x28 buttons at x=1462,1490,1516,1542,1570, y=386).
fn kakaotalk_26_10_toolbar() -> (AxFrame, Vec<AxFrame>) {
    let field = frame(1235.0, 389.0, 221.0, 22.0);
    let buttons = [1462.0, 1490.0, 1516.0, 1542.0, 1570.0]
        .iter()
        .map(|&x| frame(x, 386.0, 28.0, 28.0))
        .collect();
    (field, buttons)
}

#[test]
fn search_toolbar_buttons_structural_guard() {
    // Accepted regardless of AXIdentifier: every auto-id set seen so far, plus none at all.
    for ids in [
        ["_NS:10", "_NS:30", "_NS:43", "_NS:56", "_NS:80"],
        ["_NS:10", "_NS:31", "_NS:45", "_NS:73", "_NS:85"],
        ["_NS:10", "_NS:31", "_NS:45", "_NS:59", "_NS:85"],
        ["", "", "", "", ""],
    ] {
        let (field_frame, button_frames) = kakaotalk_26_10_toolbar();
        let mut tree = SyntheticTree::new();
        let field = tree.add("AXTextField");
        tree.frames.insert(field, field_frame);
        let mut siblings = vec![tree.add("AXButton"), field];
        for (id, f) in ids.iter().zip(button_frames) {
            let b = tree.add("AXButton");
            tree.set(b, "AXIdentifier", AxValue::String((*id).into()));
            tree.frames.insert(b, f);
            siblings.push(b);
        }
        let btns = validate_search_toolbar_buttons(&tree, field, &siblings).unwrap();
        assert_eq!(btns, siblings[2..7].to_vec());

        // A non-button among the five is rejected.
        tree.set(siblings[4], "AXRole", AxValue::String("AXStaticText".into()));
        assert_eq!(
            validate_search_toolbar_buttons(&tree, field, &siblings).unwrap_err().message(),
            "search_toolbar_layout_unknown"
        );
    }

    // Fewer than five buttons after the field.
    let mut tree = SyntheticTree::new();
    let field = tree.add("AXTextField");
    let mut siblings = vec![field];
    for _ in 0..4 {
        siblings.push(tree.add("AXButton"));
    }
    assert_eq!(
        validate_search_toolbar_buttons(&tree, field, &siblings).unwrap_err().message(),
        "search_toolbar_layout_unknown"
    );
}

#[test]
fn search_toolbar_geometry_rules() {
    let unknown = |field: Option<AxFrame>, buttons: Vec<Option<AxFrame>>| {
        check_search_toolbar_geometry(field, &buttons).unwrap_err().message().to_string()
    };
    let (field, buttons) = kakaotalk_26_10_toolbar();
    let ok: Vec<Option<AxFrame>> = buttons.iter().copied().map(Some).collect();
    check_search_toolbar_geometry(Some(field), &ok).unwrap();

    let expect = "search_toolbar_layout_unknown";
    // Missing geometry fails closed.
    assert_eq!(unknown(None, ok.clone()), expect);
    let mut missing = ok.clone();
    missing[3] = None;
    assert_eq!(unknown(Some(field), missing), expect);
    // Wrong count.
    assert_eq!(unknown(Some(field), ok[..4].to_vec()), expect);
    // A button on another row (e.g. the room header toolbar above).
    let mut other_row = ok.clone();
    other_row[2] = Some(frame(1516.0, 340.0, 28.0, 28.0));
    assert_eq!(unknown(Some(field), other_row), expect);
    // A button left of the search field.
    let mut left = ok.clone();
    left[0] = Some(frame(1200.0, 386.0, 28.0, 28.0));
    assert_eq!(unknown(Some(field), left), expect);
    // Out of left-to-right order.
    let mut swapped = ok.clone();
    swapped.swap(0, 1);
    assert_eq!(unknown(Some(field), swapped), expect);
    // Different size.
    let mut big = ok.clone();
    big[4] = Some(frame(1570.0, 380.0, 40.0, 40.0));
    assert_eq!(unknown(Some(field), big), expect);
    // Overlapping buttons.
    let mut overlap = ok.clone();
    overlap[1] = Some(frame(1470.0, 386.0, 28.0, 28.0));
    assert_eq!(unknown(Some(field), overlap), expect);
}

#[test]
fn hidden_self_chat_friends_menu_selection_and_homonym_friend_guard() {
    use kakaotalk_ax::macos_ax_logic::{
        select_self_chat_menu_item, select_self_profile_cell, verify_opened_self_window,
    };

    // Friends AXOutline (`26.10.0` bidirectional disclosure structure):
    //   row[0]: AXDisclosureLevel=0, AXDisclosedRows=[row1], _NS:96 AXStaticText="내 기본프로필"
    //   row[1]: AXDisclosureLevel=1, AXDisclosedByRow=[row0], AXCell.actions=["AXShowMenu"]
    //   row[2]: AXDisclosureLevel=0, next section header "친구 10"
    //   row[3]: AXDisclosureLevel=1, AXDisclosedByRow=[row2], friend row "나"
    let mut tree = SyntheticTree::new();
    let header_row = tree.add("AXRow");
    let header_cell = tree.add("AXCell");
    let header_label = tree.add("AXStaticText");
    tree.set(header_label, "AXValue", AxValue::String("내 기본프로필".into()));
    tree.set(header_label, "AXIdentifier", AxValue::String("_NS:96".into()));
    tree.set(header_cell, "AXChildren", AxValue::Elements(vec![header_label]));
    tree.set(header_row, "AXChildren", AxValue::Elements(vec![header_cell]));
    tree.set(header_row, "AXDisclosureLevel", AxValue::Int(0));

    let self_row = tree.add("AXRow");
    let self_cell = tree.add("AXCell");
    let self_name = tree.add("AXStaticText");
    tree.set(self_name, "AXValue", AxValue::String("이현섭".into()));
    tree.set(self_cell, "AXChildren", AxValue::Elements(vec![self_name]));
    tree.actions.insert(self_cell, vec!["AXShowMenu".into()]);
    tree.set(self_row, "AXChildren", AxValue::Elements(vec![self_cell]));
    tree.set(self_row, "AXDisclosureLevel", AxValue::Int(1));
    tree.set(self_row, "AXDisclosedByRow", AxValue::Elements(vec![header_row]));
    tree.set(header_row, "AXDisclosedRows", AxValue::Elements(vec![self_row]));

    let next_header_row = tree.add("AXRow");
    let next_header_cell = tree.add("AXCell");
    let next_header_label = tree.add("AXStaticText");
    tree.set(next_header_label, "AXValue", AxValue::String("친구 10".into()));
    tree.set(
        next_header_cell,
        "AXChildren",
        AxValue::Elements(vec![next_header_label]),
    );
    tree.set(
        next_header_row,
        "AXChildren",
        AxValue::Elements(vec![next_header_cell]),
    );
    tree.set(next_header_row, "AXDisclosureLevel", AxValue::Int(0));

    let friend_row = tree.add("AXRow");
    let friend_cell = tree.add("AXCell");
    let friend_label = tree.add("AXStaticText");
    tree.set(friend_label, "AXValue", AxValue::String("나".into()));
    tree.set(friend_cell, "AXChildren", AxValue::Elements(vec![friend_label]));
    tree.actions.insert(friend_cell, vec!["AXShowMenu".into()]);
    tree.set(friend_row, "AXChildren", AxValue::Elements(vec![friend_cell]));
    tree.set(friend_row, "AXDisclosureLevel", AxValue::Int(1));
    tree.set(friend_row, "AXDisclosedByRow", AxValue::Elements(vec![next_header_row]));
    tree.set(
        next_header_row,
        "AXDisclosedRows",
        AxValue::Elements(vec![friend_row]),
    );

    assert_eq!(
        select_self_profile_cell(&tree, &[header_row, self_row, next_header_row, friend_row])
            .unwrap(),
        self_cell
    );
    // Without the "내 기본프로필" section header, fails closed
    assert_eq!(
        select_self_profile_cell(&tree, &[self_row, next_header_row, friend_row])
            .unwrap_err()
            .message(),
        "self_room_not_verified"
    );
    // Broken reverse link (AXDisclosedByRow != header_row) fails closed
    tree.set(self_row, "AXDisclosedByRow", AxValue::Elements(vec![next_header_row]));
    assert_eq!(
        select_self_profile_cell(&tree, &[header_row, self_row, next_header_row, friend_row])
            .unwrap_err()
            .message(),
        "self_room_not_verified"
    );
    tree.set(self_row, "AXDisclosedByRow", AxValue::Elements(vec![header_row]));
    // Ambiguous 2 disclosed rows fails closed
    tree.set(
        header_row,
        "AXDisclosedRows",
        AxValue::Elements(vec![self_row, friend_row]),
    );
    assert_eq!(
        select_self_profile_cell(&tree, &[header_row, self_row, next_header_row, friend_row])
            .unwrap_err()
            .message(),
        "self_room_not_verified"
    );
    tree.set(header_row, "AXDisclosedRows", AxValue::Elements(vec![self_row]));

    // Context menu on "내 기본프로필" has unique enabled "나와의 채팅"
    let menu = tree.add("AXMenu");
    let open_self = tree.add("AXMenuItem");
    tree.set(open_self, "AXTitle", AxValue::String("나와의 채팅".into()));
    tree.set(open_self, "AXEnabled", AxValue::Bool(true));
    let other_item = tree.add("AXMenuItem");
    tree.set(other_item, "AXTitle", AxValue::String("프로필 보기".into()));
    tree.set(other_item, "AXEnabled", AxValue::Bool(true));
    tree.set(menu, "AXChildren", AxValue::Elements(vec![open_self, other_item]));
    assert_eq!(select_self_chat_menu_item(&tree, &[menu]).unwrap(), open_self);

    // Opened self-chat window "이현섭" matches exact title & self-alias
    let self_win = tree.add("AXWindow");
    tree.set(self_win, "AXTitle", AxValue::String("이현섭".into()));
    let friend_win = tree.add("AXWindow");
    tree.set(friend_win, "AXTitle", AxValue::String("나".into()));
    tree.windows = vec![self_win, friend_win];

    assert_eq!(
        verify_opened_self_window(&tree, "이현섭", None, self_win).unwrap(),
        "이현섭"
    );
    assert_eq!(
        verify_opened_self_window(&tree, "이현섭", Some(self_win), self_win).unwrap(),
        "이현섭"
    );
    assert_eq!(
        verify_opened_self_window(&tree, "나와의 채팅", None, self_win).unwrap(),
        "이현섭"
    );
    // Homonym friend room "나" already open -> official self menu focused self_win ("이현섭"),
    // NOT friend_win -> MUST reject with self_room_not_verified!
    assert_eq!(
        verify_opened_self_window(&tree, "나", Some(friend_win), self_win)
            .unwrap_err()
            .message(),
        "self_room_not_verified"
    );
    // Requesting a different non-alias title fails closed
    assert_eq!(
        verify_opened_self_window(&tree, "다른 친구", None, self_win)
            .unwrap_err()
            .message(),
        "self_room_not_verified"
    );

    // Production transition guard (`resolve_self_menu_outcome`):
    // 1. If `main` was NOT proven focused before pressing the menu (e.g. friend window stayed focused),
    //    must reject immediately with `main_window_focus_not_observed` — never accept unchanged friend window!
    use kakaotalk_ax::macos_ax_logic::resolve_self_menu_outcome;
    let main_win = 999usize;
    let eq = |a: &usize, b: &usize| a == b;
    assert_eq!(
        resolve_self_menu_outcome(
            &main_win,
            Some(&friend_win),
            &[friend_win],
            &[friend_win],
            Some(&friend_win),
            eq
        )
        .unwrap_err()
        .message(),
        "main_window_focus_not_observed"
    );
    assert_eq!(
        resolve_self_menu_outcome(&main_win, None, &[friend_win], &[friend_win], Some(&friend_win), eq)
            .unwrap_err()
            .message(),
        "main_window_focus_not_observed"
    );

    // 2. Menu press failed / no-change (focus stays on `main_win`, no new chat opened):
    //    always returns Ok(None) so polling times out cleanly — never accepted by elapsed time!
    assert_eq!(
        resolve_self_menu_outcome(
            &main_win,
            Some(&main_win),
            &[friend_win],
            &[friend_win],
            Some(&main_win),
            eq
        )
        .unwrap(),
        None
    );

    // 3. Proven `main -> existing self_win` focus transition is accepted with `already_open = true`.
    assert_eq!(
        resolve_self_menu_outcome(
            &main_win,
            Some(&main_win),
            &[self_win, friend_win],
            &[self_win, friend_win],
            Some(&self_win),
            eq
        )
        .unwrap(),
        Some((&self_win, true))
    );

    // 4. Proven newly opened chat window is accepted with `already_open = false`.
    assert_eq!(
        resolve_self_menu_outcome(
            &main_win,
            Some(&main_win),
            &[friend_win],
            &[friend_win, self_win],
            Some(&main_win),
            eq
        )
        .unwrap(),
        None
    );

    assert_eq!(
        resolve_self_menu_outcome(
            &main_win,
            Some(&main_win),
            &[friend_win],
            &[friend_win, self_win],
            Some(&self_win),
            eq
        )
        .unwrap(),
        Some((&self_win, false))
    );

    // 5. Bound AX identity verification at dispatch time (`verify_bound_self_window`):
    //    - Missing bound identity -> self_room_not_verified
    //    - Matching unique window with same AX identity -> Ok(())
    //    - Window closed & replaced by a different AX element with the same title -> target_room_window_changed
    //    - Duplicate same-title windows -> target_room_window_ambiguous
    use kakaotalk_ax::macos_ax_logic::verify_bound_self_window;
    assert_eq!(
        verify_bound_self_window(&[self_win], None, eq)
            .unwrap_err()
            .message(),
        "self_room_not_verified"
    );
    assert!(verify_bound_self_window(&[self_win], Some(&self_win), eq).is_ok());
    assert_eq!(
        verify_bound_self_window(&[friend_win], Some(&self_win), eq)
            .unwrap_err()
            .message(),
        "target_room_window_changed"
    );
    assert_eq!(
        verify_bound_self_window(&[self_win, friend_win], Some(&self_win), eq)
            .unwrap_err()
            .message(),
        "target_room_window_ambiguous"
    );
    assert_eq!(
        verify_bound_self_window(&[], Some(&self_win), eq)
            .unwrap_err()
            .message(),
        "target_room_window_not_open"
    );
}
