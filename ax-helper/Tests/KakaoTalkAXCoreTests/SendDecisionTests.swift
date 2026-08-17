import XCTest
@testable import KakaoTalkAXCore

final class SendDecisionTests: XCTestCase {
    func testUniqueExactWindowTitleIsSelectedWithoutFrontmostState() {
        XCTAssertEqual(
            uniqueExactMatchIndex(titles: ["Other room", "Requested room"], expected: "Requested room"),
            1
        )
    }

    func testMissingExactWindowTitleFailsClosed() {
        XCTAssertNil(uniqueExactMatchIndex(titles: ["Other room"], expected: "Requested room"))
    }

    func testDuplicateExactWindowTitlesFailClosed() {
        XCTAssertNil(
            uniqueExactMatchIndex(
                titles: ["Requested room", "Other room", "Requested room"],
                expected: "Requested room"
            )
        )
    }

    func testCanonicallyEquivalentButByteDifferentWindowTitleFailsClosed() {
        XCTAssertNil(uniqueExactMatchIndex(titles: ["Cafe\u{301}"], expected: "Caf\u{e9}"))
    }

    func testUniqueLabeledButtonUsesAXPress() {
        XCTAssertEqual(sendMethod(labeledButtonCount: 1), .pressLabeledButton)
    }

    func testMissingLabeledButtonUsesPlainReturnFallback() {
        XCTAssertEqual(sendMethod(labeledButtonCount: 0), .plainReturnKey)
    }

    func testAmbiguousLabeledButtonsFailClosed() {
        XCTAssertEqual(sendMethod(labeledButtonCount: 2), .refuse)
    }

    func testActivationPollingWaitsOnlyUntilBoundedDeadline() {
        let policy = ActivationHandshakePolicy(
            activationTimeout: 1.5,
            retryInterval: 0.05,
            maximumFocusAttempts: 3
        )

        XCTAssertEqual(
            policy.activationDecision(isExpectedApplicationFrontmost: false, elapsed: 1.49),
            .retry
        )
        XCTAssertEqual(
            policy.activationDecision(isExpectedApplicationFrontmost: false, elapsed: 1.5),
            .activationTimeout
        )
        XCTAssertEqual(
            policy.activationDecision(isExpectedApplicationFrontmost: true, elapsed: 1.5),
            .verified
        )
    }

    func testExactReadbackWinsWhenAXOperationReportsTransientFailure() {
        let policy = ActivationHandshakePolicy.default

        XCTAssertEqual(
            policy.focusDecision(
                axOperationReportedSuccess: false,
                exactReadbackVerified: true,
                completedAttempts: 1
            ),
            .verified
        )
    }

    func testFocusFailureRetriesOnlyToConfiguredAttemptBound() {
        let policy = ActivationHandshakePolicy(
            activationTimeout: 1.5,
            retryInterval: 0.05,
            maximumFocusAttempts: 3
        )

        XCTAssertEqual(
            policy.focusDecision(
                axOperationReportedSuccess: false,
                exactReadbackVerified: false,
                completedAttempts: 2
            ),
            .retry
        )
        XCTAssertEqual(
            policy.focusDecision(
                axOperationReportedSuccess: false,
                exactReadbackVerified: false,
                completedAttempts: 3
            ),
            .focusTimeout
        )
    }
}
