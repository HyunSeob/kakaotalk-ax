import AppKit
import ApplicationServices
import Foundation
import KakaoTalkAXCore

private let kakaoTalkBundleIdentifier = "com.kakao.KakaoTalkMac"
private let maximumInputBytes = 8_192
private let maximumAXNodes = 512
private let maximumAXDepth = 12
private let acceptedSendButtonLabels: Set<String> = ["Send", "전송"]

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

private func elementAttribute(_ element: AXUIElement, _ name: String) -> AXUIElement? {
    guard let value = attribute(element, name), CFGetTypeID(value) == AXUIElementGetTypeID() else {
        return nil
    }
    return unsafeBitCast(value, to: AXUIElement.self)
}

private func childElements(_ element: AXUIElement) -> [AXUIElement] {
    guard let value = attribute(element, kAXChildrenAttribute) else { return [] }
    return value as? [AXUIElement] ?? []
}

private func isEnabled(_ element: AXUIElement) -> Bool {
    (attribute(element, kAXEnabledAttribute) as? Bool) == true
}

private func isValueSettable(_ element: AXUIElement) -> Bool {
    var settable = DarwinBoolean(false)
    return AXUIElementIsAttributeSettable(element, kAXValueAttribute as CFString, &settable) == .success
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

private func focusedKakaoTalkWindow(expectedIdentity: String) -> AXUIElement? {
    guard let application = NSWorkspace.shared.frontmostApplication,
          application.bundleIdentifier == kakaoTalkBundleIdentifier else {
        return nil
    }
    let axApplication = AXUIElementCreateApplication(application.processIdentifier)
    guard let window = elementAttribute(axApplication, kAXFocusedWindowAttribute),
          let title = stringAttribute(window, kAXTitleAttribute),
          exactlyEqual(title, expectedIdentity) else {
        return nil
    }
    return window
}

private func uniqueComposer(in window: AXUIElement) -> AXUIElement? {
    let candidates = descendants(of: window).filter {
        stringAttribute($0, kAXRoleAttribute) == kAXTextAreaRole
            && isEnabled($0)
            && isValueSettable($0)
    }
    return candidates.count == 1 ? candidates[0] : nil
}

private func labeledSendButtons(in window: AXUIElement) -> [AXUIElement] {
    descendants(of: window).filter { element in
        guard stringAttribute(element, kAXRoleAttribute) == kAXButtonRole, isEnabled(element) else {
            return false
        }
        let labels = [
            stringAttribute(element, kAXTitleAttribute),
            stringAttribute(element, kAXDescriptionAttribute),
            stringAttribute(element, kAXHelpAttribute),
        ].compactMap { $0 }
        return labels.contains { acceptedSendButtonLabels.contains($0) }
    }
}

private func postPlainReturn(
    to composer: AXUIElement,
    expectedIdentity: String,
    expectedText: String
) -> Bool {
    guard AXUIElementSetAttributeValue(
        composer,
        kAXFocusedAttribute as CFString,
        kCFBooleanTrue
    ) == .success,
    (attribute(composer, kAXFocusedAttribute) as? Bool) == true,
    let source = CGEventSource(stateID: .combinedSessionState),
    let keyDown = CGEvent(keyboardEventSource: source, virtualKey: 36, keyDown: true),
    let keyUp = CGEvent(keyboardEventSource: source, virtualKey: 36, keyDown: false),
    let application = NSWorkspace.shared.frontmostApplication,
    application.bundleIdentifier == kakaoTalkBundleIdentifier else {
        return false
    }

    let axApplication = AXUIElementCreateApplication(application.processIdentifier)
    guard let finalWindow = elementAttribute(axApplication, kAXFocusedWindowAttribute),
          let finalTitle = stringAttribute(finalWindow, kAXTitleAttribute),
          exactlyEqual(finalTitle, expectedIdentity),
          let finalComposer = uniqueComposer(in: finalWindow),
          CFEqual(composer, finalComposer),
          (attribute(finalComposer, kAXFocusedAttribute) as? Bool) == true,
          let finalReadback = stringAttribute(finalComposer, kAXValueAttribute),
          exactlyEqual(finalReadback, expectedText) else {
        return false
    }

    keyDown.flags = []
    keyUp.flags = []
    guard let postingApplication = NSWorkspace.shared.frontmostApplication,
          postingApplication.bundleIdentifier == kakaoTalkBundleIdentifier,
          postingApplication.processIdentifier == application.processIdentifier else {
        return false
    }
    let postingAXApplication = AXUIElementCreateApplication(postingApplication.processIdentifier)
    guard let postingWindow = elementAttribute(postingAXApplication, kAXFocusedWindowAttribute),
          CFEqual(finalWindow, postingWindow),
          let postingTitle = stringAttribute(postingWindow, kAXTitleAttribute),
          exactlyEqual(postingTitle, expectedIdentity),
          let postingComposer = uniqueComposer(in: postingWindow),
          CFEqual(composer, postingComposer),
          (attribute(postingComposer, kAXFocusedAttribute) as? Bool) == true,
          let postingReadback = stringAttribute(postingComposer, kAXValueAttribute),
          exactlyEqual(postingReadback, expectedText) else {
        return false
    }

    keyDown.postToPid(postingApplication.processIdentifier)
    keyUp.postToPid(postingApplication.processIdentifier)
    return true
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
    AXIsProcessTrusted()
        && !NSRunningApplication.runningApplications(withBundleIdentifier: kakaoTalkBundleIdentifier).isEmpty
}

guard CommandLine.arguments.count == 2 else { emit("unavailable") }
let operation = CommandLine.arguments[1]

if operation == "availability" {
    emit(isAvailable() ? "available" : "unavailable")
}

guard operation == "prepare" || operation == "send",
      isAvailable(),
      let payload = requestPayload(),
      let window = focusedKakaoTalkWindow(expectedIdentity: payload.targetIdentity),
      let composer = uniqueComposer(in: window) else {
    emit("mismatch")
}

if operation == "prepare" {
    guard AXUIElementSetAttributeValue(composer, kAXValueAttribute as CFString, payload.text as CFTypeRef) == .success else {
        emit("unavailable")
    }
    guard let readback = stringAttribute(composer, kAXValueAttribute),
          exactlyEqual(readback, payload.text) else {
        emit("readback_mismatch")
    }
    emit("ready")
}

guard let readback = stringAttribute(composer, kAXValueAttribute),
      exactlyEqual(readback, payload.text) else {
    emit("mismatch")
}

let sendButtons = labeledSendButtons(in: window)
switch sendMethod(labeledButtonCount: sendButtons.count) {
case .pressLabeledButton:
    guard AXUIElementPerformAction(sendButtons[0], kAXPressAction as CFString) == .success else {
        emit("mismatch")
    }
case .plainReturnKey:
    guard postPlainReturn(
        to: composer,
        expectedIdentity: payload.targetIdentity,
        expectedText: payload.text
    ) else {
        emit("mismatch")
    }
case .refuse:
    emit("mismatch")
}
emit("invoked")
