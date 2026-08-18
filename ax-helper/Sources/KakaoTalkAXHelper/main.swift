import AppKit
import ApplicationServices
import CoreGraphics
import Foundation
import KakaoTalkAXCore

@_silgen_name("CGEventSetWindowLocation")
private func setCGEventWindowLocation(_ event: CGEvent, _ location: CGPoint)

private let kakaoTalkBundleIdentifier = "com.kakao.KakaoTalkMac"
private let maximumInputBytes = 8_192
private let maximumAXWindows = 8
private let maximumContainerNodesPerWindow = 96
private let maximumContainerDepth = 3
private let maximumRowsPerContainer = 512
private let maximumRowIdentityNodes = 12
private let maximumRowIdentityDepth = 1
private let maximumWindowContentNodes = 256
private let maximumWindowContentDepth = 6
private let maximumSearchResultNodes = 48
private let maximumSearchResultDepth = 4
private let globalSearchDeadline: TimeInterval = 2.0
private let globalSearchCancelDeadline: TimeInterval = 1.0
private let acceptedSendButtonLabels: Set<String> = ["Send", "전송"]
private let policy = BackgroundTargetPolicy.default

struct TargetWindow {
    let id: CGWindowID
    let pid: pid_t
    let bounds: CGRect
}

struct RowMatch {
    let window: AXUIElement
    let container: AXUIElement
    let row: AXUIElement
    let source: RowSource
}

enum RowSource: Equatable {
    case currentChatList
    case globalSearch
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

enum SearchFieldResolution {
    case unique(window: AXUIElement, field: AXUIElement)
    case missing
    case ambiguous
}

enum GlobalSearchWriteResult {
    case invoked
    case mismatch
    case focusUnavailable
    case clearUnavailable
    case windowUnavailable
    case eventUnavailable
}

enum TargetResolution {
    case unique(RowMatch, openedGlobalSearch: Bool)
    case failure(String, openedGlobalSearch: Bool)
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

private func windowBounds(_ window: AXUIElement) -> WindowBounds? {
    guard let positionValue = attribute(window, kAXPositionAttribute),
          CFGetTypeID(positionValue) == AXValueGetTypeID(),
          let sizeValue = attribute(window, kAXSizeAttribute),
          CFGetTypeID(sizeValue) == AXValueGetTypeID() else {
        return nil
    }
    var position = CGPoint.zero
    var size = CGSize.zero
    guard AXValueGetValue(positionValue as! AXValue, .cgPoint, &position),
          AXValueGetValue(sizeValue as! AXValue, .cgSize, &size) else {
        return nil
    }
    return WindowBounds(
        x: position.x,
        y: position.y,
        width: size.width,
        height: size.height
    )
}

private func elementFrame(_ element: AXUIElement) -> CGRect? {
    guard let positionValue = attribute(element, kAXPositionAttribute),
          CFGetTypeID(positionValue) == AXValueGetTypeID(),
          let sizeValue = attribute(element, kAXSizeAttribute),
          CFGetTypeID(sizeValue) == AXValueGetTypeID() else {
        return nil
    }
    var position = CGPoint.zero
    var size = CGSize.zero
    guard AXValueGetValue(positionValue as! AXValue, .cgPoint, &position),
          AXValueGetValue(sizeValue as! AXValue, .cgSize, &size) else {
        return nil
    }
    return CGRect(origin: position, size: size)
}

private func isVisibleRow(_ row: AXUIElement, in window: AXUIElement) -> Bool {
    guard let rowFrame = elementFrame(row),
          let bounds = windowBounds(window),
          rowFrame.width > 1,
          rowFrame.height > 1 else {
        return false
    }
    let windowFrame = CGRect(
        x: bounds.x,
        y: bounds.y,
        width: bounds.width,
        height: bounds.height
    )
    return rowFrame.intersects(windowFrame)
}

private func exactlyEqual(_ left: String, _ right: String) -> Bool {
    left.utf8.elementsEqual(right.utf8)
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
              let x = (boundsDictionary["X"] as? NSNumber)?.doubleValue,
              let y = (boundsDictionary["Y"] as? NSNumber)?.doubleValue,
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
            x: x,
            y: y,
            width: width,
            height: height
        )
    }
}

private func targetWindow(pid: pid_t, preferredBounds: WindowBounds? = nil) -> TargetWindow? {
    guard let selected = selectedWindow(
              candidates: cgWindowCandidates(),
              validatedPID: pid,
              preferredBounds: preferredBounds
          ),
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

private func waitForTargetWindow(
    pid: pid_t,
    preferredBounds: WindowBounds? = nil
) -> TargetWindow? {
    let deadline = ProcessInfo.processInfo.systemUptime + policy.windowDeadline
    repeat {
        if let window = targetWindow(pid: pid, preferredBounds: preferredBounds) { return window }
        Thread.sleep(forTimeInterval: policy.retryInterval)
    } while ProcessInfo.processInfo.systemUptime < deadline
    return nil
}

private func isTableContainer(_ element: AXUIElement) -> Bool {
    let role = stringAttribute(element, kAXRoleAttribute)
    return role == kAXTableRole || role == kAXOutlineRole || role == "AXList"
}

private func tableContainers(in window: AXUIElement) -> [AXUIElement] {
    var result: [AXUIElement] = []
    let elements = boundedBreadthFirst(
        roots: Array(childElements(window).prefix(maximumContainerNodesPerWindow)),
        maximumDepth: maximumContainerDepth,
        maximumNodes: maximumContainerNodesPerWindow
    ) { element in
        isTableContainer(element) ? [] : childElements(element)
    }
    for element in elements where isTableContainer(element) {
        appendUnique(element, to: &result)
    }
    return result
}

private func rows(in container: AXUIElement) -> [AXUIElement] {
    let attributedRows = elementArrayAttribute(container, kAXRowsAttribute)
    let candidates = attributedRows.isEmpty ? childElements(container) : attributedRows
    var result: [AXUIElement] = []
    for candidate in candidates.prefix(maximumRowsPerContainer) where
        stringAttribute(candidate, kAXRoleAttribute) == kAXRowRole {
        appendUnique(candidate, to: &result)
    }
    return result
}

private func readableStrings(_ element: AXUIElement, into result: inout [String]) {
    let names: [CFString] = [
        kAXTitleAttribute as CFString,
        kAXDescriptionAttribute as CFString,
        kAXValueAttribute as CFString,
        kAXIdentifierAttribute as CFString,
    ]
    var copiedValues: CFArray?
    guard AXUIElementCopyMultipleAttributeValues(
        element,
        names as CFArray,
        AXCopyMultipleAttributeOptions(rawValue: 0),
        &copiedValues
    ) == .success,
    let values = copiedValues as? [Any] else {
        return
    }
    for case let value as String in values {
        if !result.contains(where: { exactlyEqual($0, value) }) {
            result.append(value)
        }
    }
}

private func rowTexts(_ row: AXUIElement) -> [String] {
    var result: [String] = []
    readableStrings(row, into: &result)
    let directChildren = Array(childElements(row).prefix(maximumRowIdentityNodes))
    let cells = directChildren.filter {
        stringAttribute($0, kAXRoleAttribute) == kAXCellRole
    }
    for child in directChildren where !cells.contains(where: { CFEqual($0, child) }) {
        readableStrings(child, into: &result)
    }
    for element in boundedBreadthFirst(
        roots: cells,
        maximumDepth: maximumRowIdentityDepth,
        maximumNodes: maximumRowIdentityNodes,
        children: childElements
    ) {
        readableStrings(element, into: &result)
    }
    return result
}

private func matchingRow(in application: AXUIElement, expectedIdentity: String) -> RowResolution {
    var matches: [RowMatch] = []
    let windows = elementArrayAttribute(application, kAXWindowsAttribute)
    for window in windows.prefix(maximumAXWindows) {
        let containers = tableContainers(in: window)
        let candidates = containers.map {
            RowContainerCandidate(
                selectedRowCount: elementArrayAttribute($0, kAXSelectedRowsAttribute).count,
                selectedRowsSettable: isSettable($0, kAXSelectedRowsAttribute)
            )
        }
        for index in chatListContainerIndices(candidates: candidates) {
            let container = containers[index]
            let containerRows = rows(in: container).filter { isVisibleRow($0, in: window) }
            let decision = exactRowIdentityDecision(
                rows: containerRows.map(rowTexts),
                expected: expectedIdentity
            )
            switch decision {
            case .unique(let index):
                if !matches.contains(where: { CFEqual($0.row, containerRows[index]) }) {
                    matches.append(RowMatch(
                        window: window,
                        container: container,
                        row: containerRows[index],
                        source: .currentChatList
                    ))
                }
            case .ambiguous:
                return .ambiguous
            case .missing:
                break
            }
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

private func selectRowWithoutPress(_ match: RowMatch) -> Bool {
    guard isSettable(match.container, kAXSelectedRowsAttribute),
          AXUIElementSetAttributeValue(
              match.container,
              kAXSelectedRowsAttribute as CFString,
              [match.row] as CFTypeRef
          ) == .success else {
        return false
    }
    return selectedRowsContain(match.row, container: match.container)
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

private func postSingleClick(element: AXUIElement, to window: TargetWindow) -> Bool {
    guard let frame = elementFrame(element),
          frame.width > 1,
          frame.height > 1,
          postInternalFocusNotifications(to: window) else {
        return false
    }
    let location = CGPoint(x: frame.midX, y: frame.midY)
    let localLocation = CGPoint(
        x: location.x - window.bounds.minX,
        y: location.y - window.bounds.minY
    )
    let eventNumber = Int(truncatingIfNeeded: UInt64(Date().timeIntervalSince1970 * 1_000) & 0x7FFF_FFFF)
    func mouseEvent(type: NSEvent.EventType, eventNumber: Int, isDown: Bool) -> CGEvent? {
        guard let nsEvent = NSEvent.mouseEvent(
            with: type,
            location: location,
            modifierFlags: [],
            timestamp: 0,
            windowNumber: Int(window.id),
            context: nil,
            eventNumber: eventNumber,
            clickCount: 1,
            pressure: isDown ? 1 : 0
        ), let event = nsEvent.cgEvent else {
            return nil
        }
        event.flags = []
        event.location = location
        event.setIntegerValueField(.mouseEventButtonNumber, value: 0)
        event.setIntegerValueField(.mouseEventClickState, value: 1)
        event.setIntegerValueField(.mouseEventWindowUnderMousePointer, value: Int64(window.id))
        event.setIntegerValueField(
            .mouseEventWindowUnderMousePointerThatCanHandleThisEvent,
            value: Int64(window.id)
        )
        event.setDoubleValueField(.mouseEventPressure, value: isDown ? 1 : 0)
        setCGEventWindowLocation(event, localLocation)
        return event
    }
    guard let down = mouseEvent(type: .leftMouseDown, eventNumber: eventNumber, isDown: true),
          let up = mouseEvent(type: .leftMouseUp, eventNumber: eventNumber + 1, isDown: false) else {
        return false
    }
    for event in [down, up] {
        event.timestamp = CGEventTimestamp(mach_absolute_time())
        event.postToPid(window.pid)
        usleep(50_000)
    }
    return true
}

private func postDoubleClick(row: AXUIElement, to window: TargetWindow) -> Bool {
    guard let frame = elementFrame(row),
          frame.width > 1,
          frame.height > 1,
          postInternalFocusNotifications(to: window),
          let eventSource = CGEventSource(stateID: .hidSystemState) else {
        return false
    }
    let location = CGPoint(x: frame.midX, y: frame.midY)
    let localLocation = CGPoint(
        x: location.x - window.bounds.minX,
        y: location.y - window.bounds.minY
    )
    let eventNumber = Int(truncatingIfNeeded: UInt64(Date().timeIntervalSince1970 * 1_000) & 0x7FFF_FFFF)
    func mouseEvent(
        type: NSEvent.EventType,
        eventNumber: Int,
        clickCount: Int,
        clickState: Int64,
        pressure: Float
    ) -> CGEvent? {
        guard let nsEvent = NSEvent.mouseEvent(
            with: type,
            location: location,
            modifierFlags: [],
            timestamp: 0,
            windowNumber: Int(window.id),
            context: nil,
            eventNumber: eventNumber,
            clickCount: clickCount,
            pressure: pressure
        ), let event = nsEvent.cgEvent else {
            return nil
        }
        event.flags = []
        event.location = location
        event.setIntegerValueField(.mouseEventButtonNumber, value: 0)
        event.setIntegerValueField(.mouseEventClickState, value: clickState)
        event.setIntegerValueField(.mouseEventWindowUnderMousePointer, value: Int64(window.id))
        event.setIntegerValueField(
            .mouseEventWindowUnderMousePointerThatCanHandleThisEvent,
            value: Int64(window.id)
        )
        event.setDoubleValueField(.mouseEventPressure, value: Double(pressure))
        setCGEventWindowLocation(event, localLocation)
        return event
    }
    guard let firstDown = mouseEvent(
              type: .leftMouseDown,
              eventNumber: eventNumber,
              clickCount: 1,
              clickState: 1,
              pressure: 1
          ),
          let firstUp = mouseEvent(
              type: .leftMouseUp,
              eventNumber: eventNumber + 1,
              clickCount: 1,
              clickState: 1,
              pressure: 0
          ),
          let secondDown = mouseEvent(
              type: .leftMouseDown,
              eventNumber: eventNumber + 2,
              clickCount: 2,
              clickState: 2,
              pressure: 1
          ),
          let secondUp = mouseEvent(
              type: .leftMouseUp,
              eventNumber: eventNumber + 3,
              clickCount: 2,
              clickState: 2,
              pressure: 0
          ) else {
        return false
    }
    for event in [firstDown, firstUp, secondDown, secondUp] {
        event.timestamp = CGEventTimestamp(mach_absolute_time())
        event.postToPid(window.pid)
        usleep(50_000)
    }
    _ = eventSource
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

private func boundedWindowContent(in window: AXUIElement) -> [AXUIElement] {
    boundedBreadthFirst(
        roots: Array(childElements(window).prefix(maximumWindowContentNodes)),
        maximumDepth: maximumWindowContentDepth,
        maximumNodes: maximumWindowContentNodes
    ) { element in
        isTableContainer(element) ? [] : childElements(element)
    }
}

private let maximumConversationContentNodes = 512
private let maximumConversationRows = 64

private func conversationWindowContent(in window: AXUIElement) -> [AXUIElement] {
    boundedBreadthFirst(
        roots: Array(childElements(window).prefix(maximumWindowContentNodes)),
        maximumDepth: maximumWindowContentDepth,
        maximumNodes: maximumConversationContentNodes
    ) { element in
        if isTableContainer(element) {
            return Array(rows(in: element).prefix(maximumConversationRows))
        }
        return Array(childElements(element).prefix(maximumRowsPerContainer))
    }
}

struct GlobalSearchControl {
    let window: AXUIElement
    let button: AXUIElement
}

private func globalSearchControl(in application: AXUIElement) -> GlobalSearchControl? {
    let windows = Array(elementArrayAttribute(application, kAXWindowsAttribute).prefix(maximumAXWindows))
    let contents = windows.map(boundedWindowContent)
    let candidates = contents.map { elements in
        GlobalSearchWindowCandidate(
            hasChatroomsIdentifier: elements.contains(where: {
                stringAttribute($0, kAXRoleAttribute) == kAXButtonRole
                    && stringAttribute($0, kAXIdentifierAttribute).map {
                        exactlyEqual($0, "chatrooms")
                    } == true
            }),
            eligibleSearchButtonCount: elements.filter {
                stringAttribute($0, kAXRoleAttribute) == kAXButtonRole
                    && stringAttribute($0, kAXDescriptionAttribute).map {
                        exactlyEqual($0, "Search")
                    } == true
                    && isEnabled($0)
                    && isVisible($0)
            }.count
        )
    }
    guard case .unique(let windowIndex) = globalSearchControlDecision(windows: candidates) else {
        return nil
    }
    let buttons = contents[windowIndex].filter {
        stringAttribute($0, kAXRoleAttribute) == kAXButtonRole
            && stringAttribute($0, kAXDescriptionAttribute).map {
                exactlyEqual($0, "Search")
            } == true
            && isEnabled($0)
            && isVisible($0)
    }
    guard let button = buttons.first else { return nil }
    return GlobalSearchControl(window: windows[windowIndex], button: button)
}

private func searchFieldResolution(
    in application: AXUIElement,
    within scopeWindow: AXUIElement? = nil
) -> SearchFieldResolution {
    let windows: [AXUIElement]
    if let scopeWindow {
        windows = [scopeWindow]
    } else {
        windows = Array(elementArrayAttribute(application, kAXWindowsAttribute).prefix(maximumAXWindows))
    }
    var fields: [(window: AXUIElement, field: AXUIElement)] = []
    for window in windows {
        for element in boundedWindowContent(in: window) where
            stringAttribute(element, kAXRoleAttribute) == kAXTextFieldRole
                && isEnabled(element)
                && isVisible(element)
                && isSettable(element, kAXValueAttribute)
                && !fields.contains(where: { CFEqual($0.field, element) }) {
            fields.append((window: window, field: element))
        }
    }
    switch cardinalityDecision(count: fields.count) {
    case .unique: return .unique(window: fields[0].window, field: fields[0].field)
    case .missing: return .missing
    case .ambiguous: return .ambiguous
    }
}

private func waitForSearchField(
    in application: AXUIElement,
    within scopeWindow: AXUIElement
) -> SearchFieldResolution {
    let deadline = ProcessInfo.processInfo.systemUptime + globalSearchDeadline
    var last: SearchFieldResolution = .missing
    repeat {
        last = searchFieldResolution(in: application, within: scopeWindow)
        if case .unique = last { return last }
        if case .ambiguous = last { return last }
        Thread.sleep(forTimeInterval: policy.retryInterval)
    } while ProcessInfo.processInfo.systemUptime < deadline
    return last
}

private func focusSearchField(_ field: AXUIElement, in application: AXUIElement) -> Bool {
    let deadline = ProcessInfo.processInfo.systemUptime + 0.5
    repeat {
        if (attribute(field, kAXFocusedAttribute) as? Bool) == true {
            return true
        }
        if isSettable(field, kAXFocusedAttribute),
           AXUIElementSetAttributeValue(
               field,
               kAXFocusedAttribute as CFString,
               kCFBooleanTrue
           ) == .success,
           (attribute(field, kAXFocusedAttribute) as? Bool) == true {
            return true
        }
        if isSettable(application, kAXFocusedUIElementAttribute),
           AXUIElementSetAttributeValue(
               application,
               kAXFocusedUIElementAttribute as CFString,
               field
           ) == .success,
           let focused = attribute(application, kAXFocusedUIElementAttribute),
           CFGetTypeID(focused) == AXUIElementGetTypeID(),
           CFEqual(focused, field) {
            return true
        }
        Thread.sleep(forTimeInterval: policy.retryInterval)
    } while ProcessInfo.processInfo.systemUptime < deadline
    return false
}

private func waitForExactFieldValue(_ expected: String, field: AXUIElement) -> Bool {
    let deadline = ProcessInfo.processInfo.systemUptime + globalSearchDeadline
    repeat {
        if let value = stringAttribute(field, kAXValueAttribute), exactlyEqual(value, expected) {
            return true
        }
        Thread.sleep(forTimeInterval: policy.retryInterval)
    } while ProcessInfo.processInfo.systemUptime < deadline
    return false
}

private func writeGlobalSearchQuery(
    _ query: String,
    application: AXUIElement,
    field: AXUIElement,
    fieldWindow: AXUIElement,
    pid: pid_t
) -> GlobalSearchWriteResult {
    var activeField = field
    var activeFieldWindow = fieldWindow
    if case .unique(let freshWindow, let freshField) = searchFieldResolution(
        in: application,
        within: activeFieldWindow
    ) {
        activeFieldWindow = freshWindow
        activeField = freshField
    }
    guard let fieldBounds = windowBounds(activeFieldWindow),
          let refreshedWindow = targetWindow(pid: pid, preferredBounds: fieldBounds) else {
        return .windowUnavailable
    }
    if !focusSearchField(activeField, in: application),
       !postSingleClick(element: activeField, to: refreshedWindow) {
        return .focusUnavailable
    }
    guard AXUIElementSetAttributeValue(
        activeField,
        kAXValueAttribute as CFString,
        "" as CFTypeRef
    ) == .success,
    let cleared = stringAttribute(activeField, kAXValueAttribute),
    exactlyEqual(cleared, "") else {
        return .clearUnavailable
    }
    guard postUnicodeText(query, to: refreshedWindow) else {
        return .eventUnavailable
    }
    return waitForExactFieldValue(query, field: activeField) ? .invoked : .mismatch
}

private let searchResultRoles: Set<String> = [
    kAXTableRole,
    "AXList",
    kAXRowRole,
    kAXCellRole,
    kAXStaticTextRole,
    kAXButtonRole,
]

private func searchResultRowTexts(_ row: AXUIElement) -> [String] {
    var result: [String] = []
    let elements = boundedBreadthFirst(
        roots: [row],
        maximumDepth: maximumSearchResultDepth,
        maximumNodes: maximumSearchResultNodes
    ) { element in
        let children = childElements(element)
        return children.filter {
            guard let role = stringAttribute($0, kAXRoleAttribute) else { return false }
            return searchResultRoles.contains(role)
        }
    }
    for element in elements {
        readableStrings(element, into: &result)
    }
    return result
}

private func globalSearchResult(in application: AXUIElement, expectedIdentity: String) -> RowResolution {
    var candidates: [(window: AXUIElement, container: AXUIElement, rows: [AXUIElement], texts: [[String]])] = []
    let windows = elementArrayAttribute(application, kAXWindowsAttribute)
    for window in windows.prefix(maximumAXWindows) {
        for container in tableContainers(in: window) {
            let role = stringAttribute(container, kAXRoleAttribute)
            guard role == kAXTableRole || role == "AXList" else { continue }
            let containerRows = rows(in: container)
            candidates.append((
                window: window,
                container: container,
                rows: containerRows,
                texts: containerRows.map(searchResultRowTexts)
            ))
        }
    }
    switch globalSearchResultDecision(
        containers: candidates.map(\.texts),
        expected: expectedIdentity
    ) {
    case .unique(let containerIndex, let rowIndex):
        let candidate = candidates[containerIndex]
        return .unique(RowMatch(
            window: candidate.window,
            container: candidate.container,
            row: candidate.rows[rowIndex],
            source: .globalSearch
        ))
    case .missing:
        return .missing
    case .ambiguous:
        return .ambiguous
    }
}

private func waitForGlobalSearchResult(
    in application: AXUIElement,
    expectedIdentity: String
) -> RowResolution {
    let deadline = ProcessInfo.processInfo.systemUptime + globalSearchDeadline
    var last: RowResolution = .missing
    repeat {
        // Results and their owning window may be replaced while the query is
        // updating, so retain only the RowMatch from the latest window snapshot.
        last = globalSearchResult(in: application, expectedIdentity: expectedIdentity)
        if case .unique = last { return last }
        if case .ambiguous = last { return last }
        Thread.sleep(forTimeInterval: policy.retryInterval)
    } while ProcessInfo.processInfo.systemUptime < deadline
    return last
}

private func waitForSelectedTargetRow(
    in application: AXUIElement,
    expectedIdentity: String
) -> RowResolution {
    let deadline = ProcessInfo.processInfo.systemUptime + policy.composerDeadline
    var last: RowResolution = .missing
    repeat {
        last = matchingRow(in: application, expectedIdentity: expectedIdentity)
        switch last {
        case .unique(let match):
            if selectedRowsContain(match.row, container: match.container) { return last }
        case .ambiguous:
            return last
        case .missing:
            break
        }
        Thread.sleep(forTimeInterval: policy.retryInterval)
    } while ProcessInfo.processInfo.systemUptime < deadline
    return .missing
}

private func closeGlobalSearch(in application: AXUIElement) -> Bool {
    guard let cancelControl = globalSearchControl(in: application) else {
        return true
    }
    switch searchFieldResolution(in: application, within: cancelControl.window) {
    case .missing:
        return true
    case .ambiguous:
        return false
    case .unique:
        break
    }
    // KakaoTalk visually presents this as the explicit Cancel/toggle control
    // while retaining AXDescription == "Search". Re-fetch it from the current
    // chatrooms window instead of retaining the pre-mutation element.
    guard AXUIElementPerformAction(cancelControl.button, kAXPressAction as CFString) == .success else {
        return false
    }
    let deadline = ProcessInfo.processInfo.systemUptime + globalSearchCancelDeadline
    repeat {
        if case .missing = searchFieldResolution(in: application, within: cancelControl.window) {
            return true
        }
        Thread.sleep(forTimeInterval: policy.retryInterval)
    } while ProcessInfo.processInfo.systemUptime < deadline
    return false
}

private func resolveTargetRow(
    in application: AXUIElement,
    expectedIdentity: String,
    pid: pid_t
) -> TargetResolution {
    switch matchingRow(in: application, expectedIdentity: expectedIdentity) {
    case .unique(let match):
        return .unique(match, openedGlobalSearch: false)
    case .ambiguous:
        return .failure("target_row_ambiguous", openedGlobalSearch: false)
    case .missing:
        break
    }

    guard let searchControl = globalSearchControl(in: application),
          AXUIElementPerformAction(searchControl.button, kAXPressAction as CFString) == .success else {
        return .failure("global_search_unavailable", openedGlobalSearch: false)
    }

    let field: AXUIElement
    let fieldWindow: AXUIElement
    switch waitForSearchField(in: application, within: searchControl.window) {
    case .unique(let window, let match):
        fieldWindow = window
        field = match
    case .missing:
        return .failure("global_search_field_missing", openedGlobalSearch: true)
    case .ambiguous:
        return .failure("global_search_field_ambiguous", openedGlobalSearch: true)
    }
    switch writeGlobalSearchQuery(
        expectedIdentity,
        application: application,
        field: field,
        fieldWindow: fieldWindow,
        pid: pid
    ) {
    case .focusUnavailable:
        return .failure("global_search_field_focus_unavailable", openedGlobalSearch: true)
    case .clearUnavailable:
        return .failure("global_search_field_clear_unavailable", openedGlobalSearch: true)
    case .windowUnavailable:
        return .failure("global_search_window_unavailable", openedGlobalSearch: true)
    case .eventUnavailable:
        return .failure("global_search_event_unavailable", openedGlobalSearch: true)
    case .mismatch:
        return .failure("global_search_readback_mismatch", openedGlobalSearch: true)
    case .invoked:
        break
    }
    switch waitForGlobalSearchResult(in: application, expectedIdentity: expectedIdentity) {
    case .unique(let match):
        return .unique(match, openedGlobalSearch: true)
    case .missing:
        return .failure("global_search_result_missing", openedGlobalSearch: true)
    case .ambiguous:
        return .failure("global_search_result_ambiguous", openedGlobalSearch: true)
    }
}

private func composerResolution(in window: AXUIElement) -> ComposerResolution {
    var candidates: [AXUIElement] = []
    for element in boundedWindowContent(in: window) where
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

private func waitForComposer(in window: AXUIElement) -> ComposerResolution {
    let deadline = ProcessInfo.processInfo.systemUptime + policy.composerDeadline
    var last: ComposerResolution = .missing
    repeat {
        last = composerResolution(in: window)
        if case .unique = last { return last }
        Thread.sleep(forTimeInterval: policy.retryInterval)
    } while ProcessInfo.processInfo.systemUptime < deadline
    return last
}

enum ConversationComposerResolution {
    case unique(window: AXUIElement, composer: AXUIElement)
    case missing
    case ambiguous
}

private func windowContainsExactIdentity(_ window: AXUIElement, expected: String) -> Bool {
    if [
        stringAttribute(window, kAXTitleAttribute),
        stringAttribute(window, kAXDescriptionAttribute),
        stringAttribute(window, kAXValueAttribute),
        stringAttribute(window, kAXIdentifierAttribute),
    ].compactMap({ $0 }).contains(where: { exactlyEqual($0, expected) }) {
        return true
    }
    return boundedWindowContent(in: window).contains { element in
        [
            stringAttribute(element, kAXTitleAttribute),
            stringAttribute(element, kAXDescriptionAttribute),
            stringAttribute(element, kAXValueAttribute),
            stringAttribute(element, kAXIdentifierAttribute),
        ].compactMap({ $0 }).contains(where: { exactlyEqual($0, expected) })
    }
}

private func conversationComposerResolution(
    in application: AXUIElement,
    expectedIdentity: String
) -> ConversationComposerResolution {
    var candidates: [(window: AXUIElement, composer: AXUIElement)] = []
    for window in elementArrayAttribute(application, kAXWindowsAttribute).prefix(maximumAXWindows)
        where windowContainsExactIdentity(window, expected: expectedIdentity) {
        let strictCandidates = conversationWindowContent(in: window).filter {
            stringAttribute($0, kAXRoleAttribute) == kAXTextAreaRole
                && isEnabled($0)
                && isVisible($0)
                && isSettable($0, kAXValueAttribute)
        }
        switch cardinalityDecision(count: strictCandidates.count) {
        case .unique:
            candidates.append((window: window, composer: strictCandidates[0]))
        case .ambiguous:
            return .ambiguous
        case .missing:
            break
        }
    }
    switch conversationWindowDecision(candidateCount: candidates.count) {
    case .unique:
        return .unique(window: candidates[0].window, composer: candidates[0].composer)
    case .missing:
        return .missing
    case .ambiguous:
        return .ambiguous
    }
}

private func focusBackgroundComposer(
    in application: AXUIElement,
    expectedIdentity: String,
    pid: pid_t
) -> (window: AXUIElement, composer: AXUIElement)? {
    var candidates: [(window: AXUIElement, composer: AXUIElement, frame: CGRect)] = []
    for window in elementArrayAttribute(application, kAXWindowsAttribute).prefix(maximumAXWindows) {
        guard windowContainsExactIdentity(window, expected: expectedIdentity),
              let bounds = windowBounds(window) else { continue }
        let windowRect = CGRect(x: bounds.x, y: bounds.y, width: bounds.width, height: bounds.height)
        for composer in conversationWindowContent(in: window)
            where stringAttribute(composer, kAXRoleAttribute) == kAXTextAreaRole {
            guard let frame = elementFrame(composer), frame.width > 1, frame.height > 1,
                  frame.intersects(windowRect) else { continue }
            candidates.append((window: window, composer: composer, frame: frame))
        }
    }
    let preferredCandidates = candidates.filter {
        (attribute($0.window, kAXMainAttribute) as? Bool) == true
            || (attribute($0.window, kAXFocusedAttribute) as? Bool) == true
    }
    let pool = preferredCandidates.isEmpty ? candidates : preferredCandidates
    let ordered = pool.sorted {
        if abs($0.frame.maxY - $1.frame.maxY) > 1 { return $0.frame.maxY < $1.frame.maxY }
        return $0.frame.height < $1.frame.height
    }
    guard let candidate = ordered.last else { return nil }
    if ordered.count > 1,
       abs(ordered[ordered.count - 1].frame.maxY - ordered[ordered.count - 2].frame.maxY) <= 1 {
        return nil
    }
    guard let targetWindow = waitForTargetWindow(
        pid: pid,
        preferredBounds: windowBounds(candidate.window)
    ) else {
        return nil
    }
    guard postSingleClick(element: candidate.composer, to: targetWindow) else {
        return nil
    }
    let deadline = ProcessInfo.processInfo.systemUptime + 0.8
    repeat {
        if (attribute(candidate.composer, kAXFocusedAttribute) as? Bool) == true {
            return (window: candidate.window, composer: candidate.composer)
        }
        Thread.sleep(forTimeInterval: policy.retryInterval)
    } while ProcessInfo.processInfo.systemUptime < deadline
    return nil
}

private func waitForConversationComposer(
    in application: AXUIElement,
    expectedIdentity: String,
    pid: pid_t
) -> ConversationComposerResolution {
    let deadline = ProcessInfo.processInfo.systemUptime + policy.composerDeadline
    var last: ConversationComposerResolution = .missing
    var fallbackAttempted = false
    repeat {
        last = conversationComposerResolution(in: application, expectedIdentity: expectedIdentity)
        if case .unique = last { return last }
        if case .ambiguous = last { return last }
        if !fallbackAttempted {
            fallbackAttempted = true
            if let fallback = focusBackgroundComposer(
                in: application,
                expectedIdentity: expectedIdentity,
                pid: pid
            ) {
                return .unique(window: fallback.window, composer: fallback.composer)
            }
        }
        Thread.sleep(forTimeInterval: policy.retryInterval)
    } while ProcessInfo.processInfo.systemUptime < deadline
    return last
}

private func labeledSendButtons(in window: AXUIElement) -> [AXUIElement] {
    var result: [AXUIElement] = []
    for element in boundedWindowContent(in: window) where
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
    if direct == .success,
       let readback = stringAttribute(composer, kAXValueAttribute),
       exactlyEqual(readback, text) {
        return .invoked
    }
    if (attribute(composer, kAXFocusedAttribute) as? Bool) != true
        && !focusComposer(composer) {
        return .unavailable
    }
    guard postUnicodeText(text, to: window) else {
        return .unavailable
    }
    let deadline = ProcessInfo.processInfo.systemUptime + policy.retryInterval * 8
    repeat {
        if let readback = stringAttribute(composer, kAXValueAttribute), exactlyEqual(readback, text) {
            return .invoked
        }
        Thread.sleep(forTimeInterval: policy.retryInterval)
    } while ProcessInfo.processInfo.systemUptime < deadline
    return .mismatch
}

private func requestObject() -> [String: Any]? {
    let data = FileHandle.standardInput.readDataToEndOfFile()
    guard !data.isEmpty, data.count <= maximumInputBytes,
          let object = try? JSONSerialization.jsonObject(with: data),
          let payload = object as? [String: Any] else {
        return nil
    }
    return payload
}

private func outboundPayload(from payload: [String: Any]) -> (targetIdentity: String, text: String)? {
    guard
          Set(payload.keys) == Set(["targetIdentity", "text"]),
          let targetIdentity = payload["targetIdentity"] as? String,
          let text = payload["text"] as? String,
          !targetIdentity.isEmpty, !text.isEmpty else {
        return nil
    }
    return (targetIdentity, text)
}

private func resolvePayload(from payload: [String: Any]) -> String? {
    guard Set(payload.keys) == Set(["targetIdentity"]),
          let targetIdentity = payload["targetIdentity"] as? String,
          !targetIdentity.isEmpty else {
        return nil
    }
    return targetIdentity
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

guard operation == "prepare" || operation == "send" || operation == "resolve"
    || operation == "open_resolve" else {
    emit("mismatch")
}
guard AXIsProcessTrusted() else { emit("accessibility_permission_unavailable") }
guard let request = requestObject() else { emit("mismatch") }
let targetIdentity: String
let outbound: (targetIdentity: String, text: String)?
if operation == "resolve" || operation == "open_resolve" {
    guard let identity = resolvePayload(from: request) else { emit("mismatch") }
    targetIdentity = identity
    outbound = nil
} else {
    guard let payload = outboundPayload(from: request) else { emit("mismatch") }
    targetIdentity = payload.targetIdentity
    outbound = payload
}

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
let openedGlobalSearch: Bool
switch resolveTargetRow(
    in: axApplication,
    expectedIdentity: targetIdentity,
    pid: application.processIdentifier
) {
case .unique(let match, let opened):
    rowMatch = match
    openedGlobalSearch = opened
case .failure(let status, let opened):
    if opened { _ = closeGlobalSearch(in: axApplication) }
    emit(status)
}

if operation == "resolve" {
    guard exactRowIdentityDecision(
        rows: [rowMatch.source == .globalSearch
            ? searchResultRowTexts(rowMatch.row)
            : rowTexts(rowMatch.row)],
        expected: targetIdentity
    ) == .unique(index: 0) else {
        if openedGlobalSearch { _ = closeGlobalSearch(in: axApplication) }
        emit("global_search_readback_mismatch")
    }
    if rowMatch.source == .globalSearch {
        guard selectRowWithoutPress(rowMatch),
              selectedRowsContain(rowMatch.row, container: rowMatch.container) else {
            _ = closeGlobalSearch(in: axApplication)
            emit("target_row_selection_unavailable")
        }
    }
    guard !openedGlobalSearch || closeGlobalSearch(in: axApplication) else {
        emit("global_search_unavailable")
    }
    emit(rowMatch.source == .globalSearch ? "resolved_global_search" : "resolved_current_row")
}
let didSelect = rowMatch.source == .globalSearch
    ? selectRowWithoutPress(rowMatch)
    : selectRow(rowMatch)
guard didSelect else { emit("target_row_selection_unavailable") }
let focusDecision = rowFocusDecision(
    source: rowMatch.source == .globalSearch ? .globalSearch : .currentChatList,
    selectionReadbackVerified: selectedRowsContain(rowMatch.row, container: rowMatch.container)
)
switch focusDecision {
case .focusRow:
    guard focusRow(rowMatch.row, in: axApplication) else {
        if openedGlobalSearch { _ = closeGlobalSearch(in: axApplication) }
        emit("target_row_focus_unavailable")
    }
case .useSelectedRow:
    break
case .refuseSelection:
    if openedGlobalSearch { _ = closeGlobalSearch(in: axApplication) }
    emit("target_row_selection_unavailable")
}

// Re-resolve after global search may have added or replaced a KakaoTalk window.
// Return is delivered only to the validated KakaoTalk PID after AX selected-row
// readback. It opens/commits the row without moving the user's frontmost app.
guard let eventWindow = waitForTargetWindow(
    pid: application.processIdentifier,
    preferredBounds: windowBounds(rowMatch.window)
) else {
    if openedGlobalSearch { _ = closeGlobalSearch(in: axApplication) }
    emit("target_window_unavailable")
}
switch rowOpenMethod(source: rowMatch.source == .globalSearch ? .globalSearch : .currentChatList) {
case .pidTargetedReturn:
    guard postReturn(to: eventWindow) else {
        if openedGlobalSearch { _ = closeGlobalSearch(in: axApplication) }
        emit("target_row_event_unavailable")
    }
case .pidTargetedDoubleClick:
    guard postDoubleClick(row: rowMatch.row, to: eventWindow) else {
        if openedGlobalSearch { _ = closeGlobalSearch(in: axApplication) }
        emit("target_row_event_unavailable")
    }
case .pressVerifiedRow:
    guard AXUIElementPerformAction(rowMatch.row, kAXPressAction as CFString) == .success else {
        if openedGlobalSearch { _ = closeGlobalSearch(in: axApplication) }
        emit("target_row_event_unavailable")
    }
}
Thread.sleep(forTimeInterval: policy.retryInterval)

let composerWindow: AXUIElement
let composer: AXUIElement
if rowMatch.source == .globalSearch {
    switch waitForConversationComposer(
        in: axApplication,
        expectedIdentity: targetIdentity,
        pid: application.processIdentifier
    ) {
    case .unique(let window, let matchedComposer):
        composerWindow = window
        composer = matchedComposer
    case .missing:
        if openedGlobalSearch { _ = closeGlobalSearch(in: axApplication) }
        emit("composer_missing")
    case .ambiguous:
        if openedGlobalSearch { _ = closeGlobalSearch(in: axApplication) }
        emit("composer_ambiguous")
    }
    if openedGlobalSearch { _ = closeGlobalSearch(in: axApplication) }
} else {
    let selectedRowMatch: RowMatch
    switch waitForSelectedTargetRow(in: axApplication, expectedIdentity: targetIdentity) {
    case .unique(let match):
        selectedRowMatch = match
    case .missing, .ambiguous:
        emit("target_row_selection_readback_unavailable")
    }
    composerWindow = selectedRowMatch.window
    switch waitForComposer(in: selectedRowMatch.window) {
    case .unique(let match): composer = match
    case .missing: emit("composer_missing")
    case .ambiguous: emit("composer_ambiguous")
    }
}
let composerTargetWindow = waitForTargetWindow(
    pid: application.processIdentifier,
    preferredBounds: windowBounds(composerWindow)
) ?? eventWindow

if operation == "open_resolve" {
    emit("open_resolved")
}

guard let payload = outbound else { emit("mismatch") }
if operation == "prepare" {
    switch writeAndVerify(payload.text, composer: composer, window: composerTargetWindow) {
    case .invoked: emit("ready")
    case .mismatch: emit("composer_readback_mismatch")
    case .unavailable: emit("composer_write_unavailable")
    }
}

guard let readback = stringAttribute(composer, kAXValueAttribute),
      exactlyEqual(readback, payload.text) else {
    emit("composer_readback_mismatch")
}

let sendButtons = labeledSendButtons(in: composerWindow)
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
    guard postReturn(to: composerTargetWindow) else { emit("send_unavailable") }
case .refuse:
    emit("send_mismatch")
}
emit("invoked")
