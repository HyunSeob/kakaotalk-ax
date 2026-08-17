import XCTest
@testable import KakaoTalkAXCore

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
