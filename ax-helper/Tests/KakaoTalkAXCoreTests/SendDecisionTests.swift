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
}
