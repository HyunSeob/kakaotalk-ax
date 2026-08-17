import XCTest
@testable import KakaoTalkAXCore

final class BackgroundTargetPolicyTests: XCTestCase {
    func testProcessDecisionUsesOnlyValidatedBundleAndIsDeterministic() {
        XCTAssertEqual(
            kakaoTalkProcessDecision(candidates: [
                ProcessCandidate(pid: 40, bundleIdentifier: "other.app", terminated: false),
                ProcessCandidate(pid: 30, bundleIdentifier: "com.kakao.KakaoTalkMac", terminated: false),
                ProcessCandidate(pid: 20, bundleIdentifier: "com.kakao.KakaoTalkMac", terminated: true),
            ]),
            .use(pid: 30)
        )
        XCTAssertEqual(
            kakaoTalkProcessDecision(candidates: [
                ProcessCandidate(pid: 40, bundleIdentifier: "other.app", terminated: false)
            ]),
            .missing
        )
    }

    func testWindowDecisionRejectsOtherPIDsAndNonVisibleOrUtilityWindows() {
        let selected = selectedWindow(candidates: [
            WindowCandidate(id: 1, pid: 90, layer: 0, isOnScreen: true, alpha: 1, width: 1000, height: 900),
            WindowCandidate(id: 2, pid: 80, layer: 1, isOnScreen: true, alpha: 1, width: 900, height: 800),
            WindowCandidate(id: 3, pid: 80, layer: 0, isOnScreen: false, alpha: 1, width: 900, height: 800),
            WindowCandidate(id: 5, pid: 80, layer: 0, isOnScreen: true, alpha: 1, width: 800, height: 700),
            WindowCandidate(id: 4, pid: 80, layer: 0, isOnScreen: true, alpha: 1, width: 800, height: 700),
        ], validatedPID: 80)
        XCTAssertEqual(selected?.id, 4)
    }

    func testRowIdentityRequiresOneByteExactDescendantMatch() {
        XCTAssertEqual(
            exactRowIdentityDecision(rows: [["Other"], ["Requested", "preview"]], expected: "Requested"),
            .unique(index: 1)
        )
        XCTAssertEqual(exactRowIdentityDecision(rows: [["Other"]], expected: "Requested"), .missing)
        XCTAssertEqual(
            exactRowIdentityDecision(rows: [["Requested"], ["Requested"]], expected: "Requested"),
            .ambiguous
        )
        XCTAssertEqual(
            exactRowIdentityDecision(rows: [["Cafe\u{301}"]], expected: "Caf\u{e9}"),
            .missing
        )
    }

    func testCardinalityDistinguishesMissingUniqueAndAmbiguousComposer() {
        XCTAssertEqual(cardinalityDecision(count: 0), .missing)
        XCTAssertEqual(cardinalityDecision(count: 1), .unique(index: 0))
        XCTAssertEqual(cardinalityDecision(count: 2), .ambiguous)
    }

    func testBackgroundTargetNeverRequiresFrontmostAndLaunchNeverActivates() {
        let policy = BackgroundTargetPolicy.default
        XCTAssertFalse(policy.launchActivates)
        XCTAssertFalse(policy.requiresFrontmostApplication)
        XCTAssertTrue(policy.acceptsTarget(isFrontmost: false))
        XCTAssertTrue(policy.acceptsTarget(isFrontmost: true))
    }
}

final class SendDecisionTests: XCTestCase {
    func testUniqueLabeledButtonUsesAXPress() {
        XCTAssertEqual(sendMethod(labeledButtonCount: 1), .pressLabeledButton)
    }

    func testMissingLabeledButtonUsesPlainReturnFallback() {
        XCTAssertEqual(sendMethod(labeledButtonCount: 0), .plainReturnKey)
    }

    func testAmbiguousLabeledButtonsFailClosed() {
        XCTAssertEqual(sendMethod(labeledButtonCount: 2), .refuse)
    }
}
