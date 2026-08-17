import AppKit
import ApplicationServices
import CoreGraphics
import Foundation
import KakaoTalkAXCore

private let kakaoTalkBundleIdentifier = "com.kakao.KakaoTalkMac"
private let maximumInputBytes = 8_192
private let maximumAXNodes = 2_048
private let maximumAXDepth = 16
private let acceptedSendButtonLabels: Set<String> = ["Send", "전송"]
private let policy = BackgroundTargetPolicy.default

struct TargetWindow {
    let id: CGWindowID
    let pid: pid_t
    let bounds: CGRect
}

struct RowMatch {
    let container: AXUIElement
    let row: AXUIElement
}

enum RowResolution {
    case unique(RowMatch)
    case missing
    case ambiguous
}

enum ComposerResolution {
    case unique(AXUIElement)
    case missing
    case ambiguous
}

enum InputResult {
    case invoked
    case mismatch
    case unavailable
}

private func emit(_ status: String) -> Never {
    FileHandle.standardOutput.write(Data("{\"status\":\"\(status)\"}\n".utf8))
    exit(EXIT_SUCCESS)
}

private func attribute(_ element: AXUIElement, _ name: String) -> CFTypeRef? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, name as CFString, &value) == .success else {
        return nil
    }
    return value
}

private func stringAttribute(_ element: AXUIElement, _ name: String) -> String? {
    attribute(element, name) as? String
}

private func childElements(_ element: AXUIElement) -> [AXUIElement] {
    attribute(element, kAXChildrenAttribute) as? [AXUIElement] ?? []
}

private func elementArrayAttribute(_ element: AXUIElement, _ name: String) -> [AXUIElement] {
    attribute(element, name) as? [AXUIElement] ?? []
}

private func isEnabled(_ element: AXUIElement) -> Bool {
    (attribute(element, kAXEnabledAttribute) as? Bool) == true
}

private func isVisible(_ element: AXUIElement) -> Bool {
    (attribute(element, "AXVisible") as? Bool) != false
}

private func isSettable(_ element: AXUIElement, _ name: String) -> Bool {
    var settable = DarwinBoolean(false)
    return AXUIElementIsAttributeSettable(element, name as CFString, &settable) == .success
        && settable.boolValue
}

private func exactlyEqual(_ left: String, _ right: String) -> Bool {
    left.utf8.elementsEqual(right.utf8)
}

private func descendants(of root: AXUIElement) -> [AXUIElement] {
    var queue: [(AXUIElement, Int)] = [(root, 0)]
    var result: [AXUIElement] = []
    var cursor = 0
    while cursor < queue.count && result.count < maximumAXNodes {
        let (element, depth) = queue[cursor]
        cursor += 1
        result.append(element)
        if depth < maximumAXDepth {
            for child in childElements(element).prefix(maximumAXNodes - result.count) {
                queue.append((child, depth + 1))
            }
        }
    }
    return result
}

private func appendUnique(_ element: AXUIElement, to values: inout [AXUIElement]) {
    guard !values.contains(where: { CFEqual($0, element) }) else { return }
    values.append(element)
}

private func runningKakaoTalkApplication() -> NSRunningApplication? {
    let candidates = NSRunningApplication.runningApplications(
        withBundleIdentifier: kakaoTalkBundleIdentifier
    )
    let decision = kakaoTalkProcessDecision(candidates: candidates.map {
        ProcessCandidate(
            pid: $0.processIdentifier,
            bundleIdentifier: $0.bundleIdentifier ?? "",
            terminated: $0.isTerminated
        )
    })
    guard case .use(let pid) = decision else { return nil }
    return candidates.first { $0.processIdentifier == pid }
}

private func launchKakaoTalkWithoutActivation() -> Bool {
    guard !policy.launchActivates,
          let applicationURL = NSWorkspace.shared.urlForApplication(
              withBundleIdentifier: kakaoTalkBundleIdentifier
          ) else {
        return false
    }
    let configuration = NSWorkspace.OpenConfiguration()
    configuration.activates = false
    configuration.addsToRecentItems = false
    let deadline = ProcessInfo.processInfo.systemUptime + policy.processDeadline
    var completed = false
    var accepted = false
    NSWorkspace.shared.openApplication(at: applicationURL, configuration: configuration) { application, error in
        accepted = application?.bundleIdentifier == kakaoTalkBundleIdentifier && error == nil
        completed = true
    }
    while !completed && ProcessInfo.processInfo.systemUptime < deadline {
        _ = RunLoop.current.run(
            mode: .default,
            before: Date(timeIntervalSinceNow: policy.retryInterval)
        )
    }
    return completed && accepted
}

private func waitForKakaoTalkProcess(launchIfMissing: Bool) -> (NSRunningApplication?, String?) {
    if let application = runningKakaoTalkApplication() { return (application, nil) }
    guard launchIfMissing else { return (nil, "target_process_missing") }
    guard launchKakaoTalkWithoutActivation() else { return (nil, "target_launch_unavailable") }
    let deadline = ProcessInfo.processInfo.systemUptime + policy.processDeadline
    repeat {
        if let application = runningKakaoTalkApplication() { return (application, nil) }
        Thread.sleep(forTimeInterval: policy.retryInterval)
    } while ProcessInfo.processInfo.systemUptime < deadline
    return (nil, "target_process_missing")
}

private func cgWindowCandidates() -> [WindowCandidate] {
    let options: CGWindowListOption = [.optionAll, .excludeDesktopElements]
    guard let values = CGWindowListCopyWindowInfo(options, kCGNullWindowID) as? [[String: Any]] else {
        return []
    }
    return values.compactMap { value in
        guard let id = (value[kCGWindowNumber as String] as? NSNumber)?.uint32Value,
              let pid = (value[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value,
              let layer = (value[kCGWindowLayer as String] as? NSNumber)?.intValue,
              let boundsDictionary = value[kCGWindowBounds as String] as? [String: Any],
              let width = (boundsDictionary["Width"] as? NSNumber)?.doubleValue,
              let height = (boundsDictionary["Height"] as? NSNumber)?.doubleValue else {
            return nil
        }
        return WindowCandidate(
            id: id,
            pid: pid,
            layer: layer,
            isOnScreen: (value[kCGWindowIsOnscreen as String] as? NSNumber)?.boolValue == true,
            alpha: (value[kCGWindowAlpha as String] as? NSNumber)?.doubleValue ?? 1,
            width: width,
            height: height
        )
    }
}

private func targetWindow(pid: pid_t) -> TargetWindow? {
    guard let selected = selectedWindow(candidates: cgWindowCandidates(), validatedPID: pid),
          let values = CGWindowListCopyWindowInfo(
              [.optionIncludingWindow, .excludeDesktopElements],
              selected.id
          ) as? [[String: Any]],
          let value = values.first,
          let ownerPID = (value[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value,
          ownerPID == pid,
          let boundsDictionary = value[kCGWindowBounds as String] as? [String: Any],
          let bounds = CGRect(dictionaryRepresentation: boundsDictionary as CFDictionary) else {
        return nil
    }
    return TargetWindow(id: selected.id, pid: pid, bounds: bounds)
}

private func waitForTargetWindow(pid: pid_t) -> TargetWindow? {
    let deadline = ProcessInfo.processInfo.systemUptime + policy.windowDeadline
    repeat {
        if let window = targetWindow(pid: pid) { return window }
        Thread.sleep(forTimeInterval: policy.retryInterval)
    } while ProcessInfo.processInfo.systemUptime < deadline
    return nil
}

private func rowTexts(_ row: AXUIElement) -> [String] {
    var result: [String] = []
    for element in descendants(of: row) {
        for name in [kAXTitleAttribute, kAXDescriptionAttribute, kAXValueAttribute] {
            if let value = stringAttribute(element, name),
               !result.contains(where: { exactlyEqual($0, value) }) {
                result.append(value)
            }
        }
    }
    return result
}

private func matchingRow(in application: AXUIElement, expectedIdentity: String) -> RowResolution {
    let containers = descendants(of: application).filter {
        let role = stringAttribute($0, kAXRoleAttribute)
        return role == kAXTableRole || role == kAXOutlineRole || role == "AXList"
    }
    var matches: [RowMatch] = []
    for container in containers {
        var rows: [AXUIElement] = []
        for candidate in descendants(of: container) where
            stringAttribute(candidate, kAXRoleAttribute) == kAXRowRole {
            appendUnique(candidate, to: &rows)
        }
        let decision = exactRowIdentityDecision(
            rows: rows.map(rowTexts),
            expected: expectedIdentity
        )
        switch decision {
        case .unique(let index):
            if !matches.contains(where: { CFEqual($0.row, rows[index]) }) {
                matches.append(RowMatch(container: container, row: rows[index]))
            }
        case .ambiguous:
            return .ambiguous
        case .missing:
            break
        }
    }
    switch matches.count {
    case 0: return .missing
    case 1: return .unique(matches[0])
    default: return .ambiguous
    }
}

private func selectedRowsContain(_ row: AXUIElement, container: AXUIElement) -> Bool {
    elementArrayAttribute(container, kAXSelectedRowsAttribute).contains { CFEqual($0, row) }
}

private func selectRow(_ match: RowMatch) -> Bool {
    if isSettable(match.container, kAXSelectedRowsAttribute),
       AXUIElementSetAttributeValue(
           match.container,
           kAXSelectedRowsAttribute as CFString,
           [match.row] as CFTypeRef
       ) == .success,
       selectedRowsContain(match.row, container: match.container) {
        return true
    }
    guard AXUIElementPerformAction(match.row, kAXPressAction as CFString) == .success else {
        return false
    }
    return selectedRowsContain(match.row, container: match.container)
        || (attribute(match.row, kAXSelectedAttribute) as? Bool) == true
}

private func focusRow(_ row: AXUIElement, in application: AXUIElement) -> Bool {
    if isSettable(row, kAXFocusedAttribute),
       AXUIElementSetAttributeValue(
           row,
           kAXFocusedAttribute as CFString,
           kCFBooleanTrue
       ) == .success,
       (attribute(row, kAXFocusedAttribute) as? Bool) == true {
        return true
    }
    guard isSettable(application, kAXFocusedUIElementAttribute),
          AXUIElementSetAttributeValue(
              application,
              kAXFocusedUIElementAttribute as CFString,
              row
          ) == .success,
          let focused = attribute(application, kAXFocusedUIElementAttribute),
          CFGetTypeID(focused) == AXUIElementGetTypeID() else {
        return false
    }
    return CFEqual(focused, row)
}

private func appKitNotificationEvent(type: UInt, subtype: Int16, windowID: CGWindowID) -> CGEvent? {
    guard let eventType = NSEvent.EventType(rawValue: type),
          let nsEvent = NSEvent.otherEvent(
              with: eventType,
              location: .zero,
              modifierFlags: type == 13 ? NSEvent.ModifierFlags(rawValue: 0xC0000) : [],
              timestamp: 0,
              windowNumber: Int(windowID),
              context: nil,
              subtype: subtype,
              data1: 0,
              data2: 0
          ) else {
        return nil
    }
    return nsEvent.cgEvent
}

private func postInternalFocusNotifications(to window: TargetWindow) -> Bool {
    // Mirrors the private ranking-challenge-native-runner pattern: synthesize
    // AppKit's internal activation/key-focus notifications for the validated
    // target PID without making it the system frontmost application.
    guard let activated = appKitNotificationEvent(type: 13, subtype: 1, windowID: window.id),
          let focusReturned = appKitNotificationEvent(
              type: 21,
              subtype: Int16(bitPattern: 0x8000),
              windowID: 0
          ) else {
        return false
    }
    for event in [activated, focusReturned] {
        event.timestamp = CGEventTimestamp(mach_absolute_time())
        event.postToPid(window.pid)
        usleep(30_000)
    }
    return true
}

private func postReturn(to window: TargetWindow) -> Bool {
    guard postInternalFocusNotifications(to: window),
          let source = CGEventSource(stateID: .hidSystemState),
          let down = CGEvent(keyboardEventSource: source, virtualKey: 36, keyDown: true),
          let up = CGEvent(keyboardEventSource: source, virtualKey: 36, keyDown: false) else {
        return false
    }
    for event in [down, up] {
        event.flags = []
        event.timestamp = CGEventTimestamp(mach_absolute_time())
        event.postToPid(window.pid)
        usleep(30_000)
    }
    return true
}

private func postUnicodeText(_ text: String, to window: TargetWindow) -> Bool {
    guard postInternalFocusNotifications(to: window),
          let source = CGEventSource(stateID: .hidSystemState) else {
        return false
    }
    let units = Array(text.utf16)
    guard !units.isEmpty else { return false }
    var offset = 0
    while offset < units.count {
        let end = min(offset + 20, units.count)
        let chunk = Array(units[offset..<end])
        guard let down = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: true),
              let up = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: false) else {
            return false
        }
        chunk.withUnsafeBufferPointer { buffer in
            down.keyboardSetUnicodeString(stringLength: buffer.count, unicodeString: buffer.baseAddress!)
        }
        for event in [down, up] {
            event.flags = []
            event.timestamp = CGEventTimestamp(mach_absolute_time())
            event.postToPid(window.pid)
            usleep(20_000)
        }
        offset = end
    }
    return true
}

private func composerResolution(in application: AXUIElement) -> ComposerResolution {
    var candidates: [AXUIElement] = []
    for element in descendants(of: application) where
        stringAttribute(element, kAXRoleAttribute) == kAXTextAreaRole
            && isEnabled(element)
            && isVisible(element)
            && isSettable(element, kAXValueAttribute) {
        appendUnique(element, to: &candidates)
    }
    switch cardinalityDecision(count: candidates.count) {
    case .unique: return .unique(candidates[0])
    case .missing: return .missing
    case .ambiguous: return .ambiguous
    }
}

private func waitForComposer(in application: AXUIElement) -> ComposerResolution {
    let deadline = ProcessInfo.processInfo.systemUptime + policy.composerDeadline
    var last: ComposerResolution = .missing
    repeat {
        last = composerResolution(in: application)
        if case .unique = last { return last }
        Thread.sleep(forTimeInterval: policy.retryInterval)
    } while ProcessInfo.processInfo.systemUptime < deadline
    return last
}

private func labeledSendButtons(in application: AXUIElement) -> [AXUIElement] {
    var result: [AXUIElement] = []
    for element in descendants(of: application) where
        stringAttribute(element, kAXRoleAttribute) == kAXButtonRole
            && isEnabled(element)
            && isVisible(element) {
        let labels = [
            stringAttribute(element, kAXTitleAttribute),
            stringAttribute(element, kAXDescriptionAttribute),
            stringAttribute(element, kAXHelpAttribute),
        ].compactMap { $0 }
        if labels.contains(where: acceptedSendButtonLabels.contains) {
            appendUnique(element, to: &result)
        }
    }
    return result
}

private func focusComposer(_ composer: AXUIElement) -> Bool {
    guard isSettable(composer, kAXFocusedAttribute),
          AXUIElementSetAttributeValue(
              composer,
              kAXFocusedAttribute as CFString,
              kCFBooleanTrue
          ) == .success else {
        return false
    }
    return (attribute(composer, kAXFocusedAttribute) as? Bool) == true
}

private func writeAndVerify(_ text: String, composer: AXUIElement, window: TargetWindow) -> InputResult {
    let direct = AXUIElementSetAttributeValue(
        composer,
        kAXValueAttribute as CFString,
        text as CFTypeRef
    )
    if direct != .success {
        guard focusComposer(composer), postUnicodeText(text, to: window) else {
            return .unavailable
        }
    }
    guard let readback = stringAttribute(composer, kAXValueAttribute) else {
        return .unavailable
    }
    return exactlyEqual(readback, text) ? .invoked : .mismatch
}

private func requestPayload() -> (targetIdentity: String, text: String)? {
    let data = FileHandle.standardInput.readDataToEndOfFile()
    guard !data.isEmpty, data.count <= maximumInputBytes,
          let object = try? JSONSerialization.jsonObject(with: data),
          let payload = object as? [String: Any],
          Set(payload.keys) == Set(["targetIdentity", "text"]),
          let targetIdentity = payload["targetIdentity"] as? String,
          let text = payload["text"] as? String,
          !targetIdentity.isEmpty, !text.isEmpty else {
        return nil
    }
    return (targetIdentity, text)
}

private func isAvailable() -> Bool {
    guard AXIsProcessTrusted() else { return false }
    return runningKakaoTalkApplication() != nil
        || NSWorkspace.shared.urlForApplication(withBundleIdentifier: kakaoTalkBundleIdentifier) != nil
}

guard CommandLine.arguments.count == 2 else { emit("unavailable") }
let operation = CommandLine.arguments[1]

if operation == "availability" {
    emit(isAvailable() ? "available" : "unavailable")
}

guard operation == "prepare" || operation == "send" else { emit("mismatch") }
guard AXIsProcessTrusted() else { emit("accessibility_permission_unavailable") }
guard let payload = requestPayload() else { emit("mismatch") }

let (resolvedApplication, processFailure) = waitForKakaoTalkProcess(launchIfMissing: true)
if let processFailure { emit(processFailure) }
guard let application = resolvedApplication else { emit("target_process_missing") }
guard application.bundleIdentifier == kakaoTalkBundleIdentifier else { emit("target_process_missing") }
guard let window = waitForTargetWindow(pid: application.processIdentifier) else {
    emit("target_window_unavailable")
}
guard policy.acceptsTarget(isFrontmost: application.isActive) else { emit("target_window_unavailable") }

let axApplication = AXUIElementCreateApplication(application.processIdentifier)
let rowMatch: RowMatch
switch matchingRow(in: axApplication, expectedIdentity: payload.targetIdentity) {
case .unique(let match): rowMatch = match
case .missing: emit("target_row_missing")
case .ambiguous: emit("target_row_ambiguous")
}
guard selectRow(rowMatch) else { emit("target_row_selection_unavailable") }
guard focusRow(rowMatch.row, in: axApplication) else {
    emit("target_row_selection_unavailable")
}

// Return is delivered only to the validated KakaoTalk PID after AX selected-row
// readback. It opens/commits the row without moving the user's frontmost app.
guard postReturn(to: window) else { emit("target_row_selection_unavailable") }
Thread.sleep(forTimeInterval: policy.retryInterval)

let composer: AXUIElement
switch waitForComposer(in: axApplication) {
case .unique(let match): composer = match
case .missing: emit("composer_missing")
case .ambiguous: emit("composer_ambiguous")
}
guard case .unique(let selectedMatch) = matchingRow(
    in: axApplication,
    expectedIdentity: payload.targetIdentity
), CFEqual(selectedMatch.row, rowMatch.row),
selectedRowsContain(rowMatch.row, container: rowMatch.container) else {
    emit("target_row_selection_unavailable")
}

if operation == "prepare" {
    switch writeAndVerify(payload.text, composer: composer, window: window) {
    case .invoked: emit("ready")
    case .mismatch: emit("composer_readback_mismatch")
    case .unavailable: emit("composer_write_unavailable")
    }
}

guard let readback = stringAttribute(composer, kAXValueAttribute),
      exactlyEqual(readback, payload.text) else {
    emit("composer_readback_mismatch")
}

let sendButtons = labeledSendButtons(in: axApplication)
switch sendMethod(labeledButtonCount: sendButtons.count) {
case .pressLabeledButton:
    guard let finalReadback = stringAttribute(composer, kAXValueAttribute),
          exactlyEqual(finalReadback, payload.text) else {
        emit("send_mismatch")
    }
    guard AXUIElementPerformAction(sendButtons[0], kAXPressAction as CFString) == .success else {
        emit("send_unavailable")
    }
case .plainReturnKey:
    guard focusComposer(composer) else { emit("send_mismatch") }
    guard let finalReadback = stringAttribute(composer, kAXValueAttribute),
          exactlyEqual(finalReadback, payload.text) else {
        emit("send_mismatch")
    }
    guard postReturn(to: window) else { emit("send_unavailable") }
case .refuse:
    emit("send_mismatch")
}
emit("invoked")
