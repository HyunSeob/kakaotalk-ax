"""Compile production Swift decisions with synthetic AX elements, without live UI."""

import pathlib
import re
import shutil
import subprocess
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]
SOURCE = ROOT / "src/kakaotalk_native/ControlKakaoAX.swift"


class ControlMatchingTests(unittest.TestCase):
    def compile_and_run(self, harness):
        compiler = shutil.which("swiftc")
        self.assertIsNotNone(compiler, "Swift compiler required; install Apple command-line tools")
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory)
            (path / "test.swift").write_text(harness, encoding="utf-8")
            build = subprocess.run([compiler, str(path / "test.swift"), "-o", str(path / "test")],
                                   capture_output=True, text=True, timeout=60)
            self.assertEqual(build.returncode, 0, build.stderr)
            result = subprocess.run([str(path / "test")], capture_output=True, text=True, timeout=5)
            self.assertEqual(result.returncode, 0, result.stderr)
            return result.stdout.splitlines()

    def test_menu_filter_and_uniqueness_guard(self):
        source = SOURCE.read_text(encoding="utf-8")
        expression = re.search(r"openItems = (menus\.flatMap.*?\n            \})", source, re.S)
        self.assertIsNotNone(expression)
        guard = re.search(r'guard openItems.count == 1 else \{.*?\}', source)
        self.assertIsNotNone(guard)
        harness = '''import Foundation
struct Element { let role: String; let identifier: String; let title: String; let enabled: Any? }
enum ControlFailure: Error { case blocked(String) }
func elements(_ list: [Element]) -> [Element] { list }
func string(_ item: Element, _ key: String) -> String {
    switch key {
    case "AXRole": return item.role
    case "AXIdentifier": return item.identifier
    case "AXTitle": return item.title
    default: return ""
    }
}
func attr(_ item: Element, _ key: String) -> Any? { key == "AXEnabled" ? item.enabled : nil }
func select(_ menus: [[Element]]) throws -> String {
    let openItems = FILTER
    GUARD
    return openItems[0].title
}
func item(_ title: String, _ identifier: String = "openChatRoom:",
          _ enabled: Any? = true, _ role: String = "AXMenuItem") -> Element {
    Element(role: role, identifier: identifier, title: title, enabled: enabled)
}
let cases: [[[Element]]] = [
    [[item("Localized alternative")]], [[item("Open Chat")]], [[item("채팅방 열기")]], [[item("")]],
    [[item("Open Chat", "other:")]], [[item("Open Chat", "openChatRoom:", false)]],
    [[item("Open Chat", "openChatRoom:", true, "AXButton")]],
    [[item("Open Chat", "openChatRoom:", nil)]], [[item("Open Chat", "openChatRoom:", "true")]],
    [[item("One"), item("Two")]], [[item("One")], [item("Two")]], [[]], [],
    [[item("Disabled", "openChatRoom:", false), item("Open Chat")]]
]
for menus in cases {
    do { print("selected:" + (try select(menus))) }
    catch ControlFailure.blocked(let reason) { print(reason) }
}
'''.replace("FILTER", expression.group(1)).replace("GUARD", guard.group(0))
        self.assertEqual(self.compile_and_run(harness), [
            "selected:Localized alternative", "selected:Open Chat", "selected:채팅방 열기", "selected:",
            *(["open_menu_item_not_unique"] * 9), "selected:Open Chat",
        ])

    def test_room_lookup_preserves_metadata_and_require_self(self):
        source = SOURCE.read_text(encoding="utf-8")
        function = source[source.index("func roomEntries("):source.index("func searchField(")]
        # Include the actual call, exact-name filter, guards and already-open branch.
        lookup = source[source.index("        let listing = try roomEntries(main"):
                        source.index("        guard !elements(cell).contains")]
        harness = '''import Foundation
typealias CFString = String
typealias CFIndex = Int
enum AXError { case success }
enum ControlFailure: Error { case blocked(String) }
class AXUIElement {
    let role: String; var value: String; var children: [AXUIElement]
    init(_ role: String, _ value: String = "", _ children: [AXUIElement] = []) {
        self.role = role; self.value = value; self.children = children
    }
}
let name = AXUIElement("AXStaticText", "Fixture group")
let time = AXUIElement("AXStaticText", "12:34")
let preview = AXUIElement("AXTextArea", "Synthetic preview")
let badge = AXUIElement("AXImage", "badge me")
let cell = AXUIElement("AXCell", "", [name, time, preview, badge])
let row = AXUIElement("AXRow", "", [cell])
let table = AXUIElement("AXTable", "", [row])
let main = AXUIElement("AXWindow")
let application = AXUIElement("AXApplication")
var metadataAllowed = false
var metadataReads = 0
func check() throws {}
func nodes(_ element: AXUIElement, tables: Bool = false) throws -> [AXUIElement] {
    if element === main { return [table] }
    precondition(metadataAllowed && element === cell, "unexpected descendant traversal")
    return element.children
}
func elements(_ element: AXUIElement, _ key: String = "AXChildren") -> [AXUIElement] {
    element.children
}
func string(_ element: AXUIElement, _ key: String) -> String {
    if key == "AXRole" { return element.role }
    if element === name && key == "AXValue" { return element.value }
    precondition(metadataAllowed, "non-name attribute requested")
    metadataReads += 1
    return element.value
}
func attr(_ element: AXUIElement, _ key: String) -> Any? {
    if element === table && key == "AXVisibleRows" { return table.children }
    precondition(metadataAllowed && element === row && key == "AXIndex")
    return NSNumber(value: 7)
}
func window(_ application: AXUIElement, _ title: String) throws -> AXUIElement { main }
func AXUIElementGetAttributeValueCount(_ element: AXUIElement, _ key: CFString,
                                      _ count: inout CFIndex) -> AXError {
    count = table.children.count; return .success
}
FUNCTION
func lookup(_ command: String, _ request: [String: Any] = [:]) throws -> [String: Any] {
    let title = "Fixture group"
    LOOKUP
    return ["name": matching[0].info["name"]!, "cellMatched": cell === matching[0].cell]
}
// Ordinary open must not traverse or read preview, time or self-badge metadata.
for request: [String: Any] in [[:], ["requireSelf": false]] {
    let result = try lookup("open", request)
    precondition(result["name"] as? String == "Fixture group")
    precondition(result["cellMatched"] as? Bool == true)
}
precondition(metadataReads == 0)
// Missing labels, empty names and row-count boundaries retain the existing guards.
name.value = ""
let emptyEntries = try roomEntries(main, namesOnly: true).entries
precondition(emptyEntries.isEmpty)
name.value = "Fixture group"
cell.children = [preview]
let missingEntries = try roomEntries(main, namesOnly: true).entries
precondition(missingEntries.isEmpty)
cell.children = [name, time, preview, badge]
table.children = []
let emptyTable = try roomEntries(main, namesOnly: true).entries
precondition(emptyTable.isEmpty)
table.children = Array(repeating: row, count: 1000)
let boundaryEntries = try roomEntries(main, namesOnly: true).entries
precondition(boundaryEntries.count == 1000)
table.children.append(row)
do {
    _ = try roomEntries(main, namesOnly: true)
    fatalError("row limit bypassed")
} catch ControlFailure.blocked(let reason) { precondition(reason == "room_list_limit") }
table.children = [row]
metadataAllowed = true
func verifyMetadata(_ info: [String: Any]) {
    precondition(info["name"] as? String == "Fixture group")
    precondition(info["isSelf"] as? Bool == true)
    precondition(info["rowIndex"] as? Int == 7)
    precondition(info["labels"] as? [String] == ["Fixture group", "12:34"])
    precondition(info["preview"] as? String == "Synthetic preview")
}
verifyMetadata(try roomEntries(main).entries[0].info)
for command in ["rooms", "search_rooms"] {
    let result = try lookup(command)
    verifyMetadata((result["items"] as! [[String: Any]])[0])
    precondition(result["reportedRowCount"] as? Int == 1)
}
_ = try lookup("open", ["requireSelf": true])
badge.value = "other badge"
do {
    _ = try lookup("open", ["requireSelf": true])
    fatalError("unverified self room accepted")
} catch ControlFailure.blocked(let reason) { precondition(reason == "self_room_not_verified") }
print("ok")
'''.replace("FUNCTION", function).replace("LOOKUP", lookup)
        self.assertEqual(self.compile_and_run(harness), ["ok"])

    def test_exact_room_guards_and_already_open_path(self):
        source = SOURCE.read_text(encoding="utf-8")
        guards = source[source.index("        guard !title.isEmpty else"):
                        source.index("        let cell = matching[0].cell")]
        harness = '''import Foundation
enum ControlFailure: Error { case blocked(String) }
struct Entry { let info: [String: Any] }
struct Element { let title: String }
var openedWindows: [Element] = []
let application = Element(title: "")
func elements(_ element: Element, _ key: String) -> [Element] { openedWindows }
func string(_ element: Element, _ key: String) -> String { element.title }
func window(_ application: Element, _ title: String) throws -> Element {
    let matching = openedWindows.filter { $0.title == title }
    guard matching.count == 1 else { throw ControlFailure.blocked("window_not_unique") }
    return matching[0]
}
func lookup(_ names: [String], _ alreadyOpen: Bool = false, _ requireSelf: Bool = false,
            _ title: String = "Exact fixture") throws -> [String: Any] {
    let entries = names.map { Entry(info: ["name": $0, "isSelf": false]) }
    let request: [String: Any] = ["requireSelf": requireSelf]
    openedWindows = alreadyOpen ? [Element(title: "Exact fixture")] : []
    GUARDS
    return ["status": "ok", "alreadyOpen": false]
}
for (names, opened, requireSelf, title) in [
    (["Exact fixture"], false, false, "Exact fixture"),
    (["Exact fixture", "Exact fixture"], false, false, "Exact fixture"),
    ([], false, false, "Exact fixture"), (["Exact fixture suffix"], false, false, "Exact fixture"),
    (["Other"], false, false, "Exact fixture"), ([], true, false, "Exact fixture"),
    (["Exact fixture", "Exact fixture"], true, false, "Exact fixture"),
    ([], true, true, "Exact fixture"), (["Exact fixture"], true, true, "Exact fixture"),
    (["Exact fixture"], false, false, "")
] {
    do { print((try lookup(names, opened, requireSelf, title))["alreadyOpen"] as! Bool) }
    catch ControlFailure.blocked(let reason) { print(reason) }
}
'''.replace("GUARDS", guards)
        self.assertEqual(self.compile_and_run(harness), [
            "false", "target_room_name_ambiguous", "room_not_in_exposed_list",
            "room_not_in_exposed_list", "room_not_in_exposed_list", "true",
            "target_room_name_ambiguous", "room_not_in_exposed_list", "self_room_not_verified",
            "exact_window_title_required",
        ])


if __name__ == "__main__":
    unittest.main()
