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

    func testWindowDecisionPrefersValidatedPIDWindowMatchingAXBounds() {
        let selected = selectedWindow(
            candidates: [
                WindowCandidate(id: 1, pid: 80, layer: 0, isOnScreen: true, alpha: 1, x: 0, y: 0, width: 1200, height: 900),
                WindowCandidate(id: 2, pid: 80, layer: 0, isOnScreen: true, alpha: 1, x: 100, y: 120, width: 700, height: 600),
                WindowCandidate(id: 3, pid: 90, layer: 0, isOnScreen: true, alpha: 1, x: 100, y: 120, width: 700, height: 600),
            ],
            validatedPID: 80,
            preferredBounds: WindowBounds(x: 100, y: 120, width: 700, height: 600)
        )
        XCTAssertEqual(selected?.id, 2)
        XCTAssertNil(selectedWindow(
            candidates: [
                WindowCandidate(id: 1, pid: 80, layer: 0, isOnScreen: true, alpha: 1, x: 0, y: 0, width: 1200, height: 900)
            ],
            validatedPID: 80,
            preferredBounds: WindowBounds(x: 100, y: 120, width: 700, height: 600)
        ))
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

final class GlobalSearchDecisionTests: XCTestCase {
    func testGlobalSearchControlRequiresOneEligibleControlInOneChatroomsWindow() {
        XCTAssertEqual(
            globalSearchControlDecision(windows: [
                GlobalSearchWindowCandidate(hasChatroomsIdentifier: true, eligibleSearchButtonCount: 1),
                GlobalSearchWindowCandidate(hasChatroomsIdentifier: false, eligibleSearchButtonCount: 1),
            ]),
            .unique(windowIndex: 0)
        )
        XCTAssertEqual(
            globalSearchControlDecision(windows: [
                GlobalSearchWindowCandidate(hasChatroomsIdentifier: true, eligibleSearchButtonCount: 0)
            ]),
            .unavailable
        )
        XCTAssertEqual(
            globalSearchControlDecision(windows: [
                GlobalSearchWindowCandidate(hasChatroomsIdentifier: true, eligibleSearchButtonCount: 2)
            ]),
            .unavailable
        )
        XCTAssertEqual(
            globalSearchControlDecision(windows: [
                GlobalSearchWindowCandidate(hasChatroomsIdentifier: true, eligibleSearchButtonCount: 1),
                GlobalSearchWindowCandidate(hasChatroomsIdentifier: true, eligibleSearchButtonCount: 1),
            ]),
            .unavailable
        )
        XCTAssertEqual(
            globalSearchControlDecision(windows: [
                GlobalSearchWindowCandidate(hasChatroomsIdentifier: true, eligibleSearchButtonCount: 1),
                GlobalSearchWindowCandidate(hasChatroomsIdentifier: true, eligibleSearchButtonCount: 0),
            ]),
            .unavailable
        )
    }

    func testGlobalSearchResultRequiresOneByteExactRowAcrossContainers() {
        XCTAssertEqual(
            globalSearchResultDecision(
                containers: [[["Other"], ["Requested"]], [["Unrelated"]]],
                expected: "Requested"
            ),
            .unique(containerIndex: 0, rowIndex: 1)
        )
        XCTAssertEqual(
            globalSearchResultDecision(containers: [[["Other"]]], expected: "Requested"),
            .missing
        )
        XCTAssertEqual(
            globalSearchResultDecision(
                containers: [[["Requested"]], [["Requested"]]],
                expected: "Requested"
            ),
            .ambiguous
        )
        XCTAssertEqual(
            globalSearchResultDecision(containers: [[["Cafe\u{301}"]]], expected: "Caf\u{e9}"),
            .missing
        )
    }
}

final class PostResolutionDecisionTests: XCTestCase {
    func testGlobalSearchSkipsUnsettablyFocusedRowOnlyAfterSelectionReadback() {
        XCTAssertEqual(
            rowFocusDecision(source: .currentChatList, selectionReadbackVerified: true),
            .focusRow
        )
        XCTAssertEqual(
            rowFocusDecision(source: .globalSearch, selectionReadbackVerified: true),
            .useSelectedRow
        )
        XCTAssertEqual(
            rowFocusDecision(source: .globalSearch, selectionReadbackVerified: false),
            .refuseSelection
        )
        XCTAssertEqual(rowOpenMethod(source: .currentChatList), .pidTargetedReturn)
        XCTAssertEqual(rowOpenMethod(source: .globalSearch), .pidTargetedDoubleClick)
    }

    func testConversationWindowDecisionRequiresExactlyOneCandidate() {
        XCTAssertEqual(conversationWindowDecision(candidateCount: 0), .missing)
        XCTAssertEqual(conversationWindowDecision(candidateCount: 1), .unique)
        XCTAssertEqual(conversationWindowDecision(candidateCount: 2), .ambiguous)
    }

    func testPostResolutionFailureStatusIdentifiesExactFailedStage() {
        XCTAssertEqual(
            postResolutionFailureStatus(
                selected: false,
                focused: false,
                eventPosted: false,
                selectionReadbackVerified: false
            ),
            "target_row_selection_unavailable"
        )
        XCTAssertEqual(
            postResolutionFailureStatus(
                selected: true,
                focused: false,
                eventPosted: false,
                selectionReadbackVerified: false
            ),
            "target_row_focus_unavailable"
        )
        XCTAssertEqual(
            postResolutionFailureStatus(
                selected: true,
                focused: true,
                eventPosted: false,
                selectionReadbackVerified: false
            ),
            "target_row_event_unavailable"
        )
        XCTAssertEqual(
            postResolutionFailureStatus(
                selected: true,
                focused: true,
                eventPosted: true,
                selectionReadbackVerified: false
            ),
            "target_row_selection_readback_unavailable"
        )
        XCTAssertNil(
            postResolutionFailureStatus(
                selected: true,
                focused: true,
                eventPosted: true,
                selectionReadbackVerified: true
            )
        )
    }
}

final class BoundedTraversalTests: XCTestCase {
    private struct Node: Equatable {
        let name: String
        let children: [Node]
    }

    func testTraversalDoesNotReadChildrenPastMaximumDepth() {
        let forbidden = Node(name: "dynamic-message-subtree", children: [])
        let root = Node(
            name: "row",
            children: [Node(name: "cell", children: [forbidden])]
        )
        var expanded: [String] = []

        let visited = boundedBreadthFirst(
            roots: [root],
            maximumDepth: 1,
            maximumNodes: 10
        ) { node in
            expanded.append(node.name)
            return node.children
        }

        XCTAssertEqual(visited.map(\.name), ["row", "cell"])
        XCTAssertEqual(expanded, ["row"])
    }

    func testTraversalStopsExpandingWhenNodeBudgetIsExhausted() {
        let root = Node(
            name: "window",
            children: (0..<20).map { Node(name: "branch-\($0)", children: []) }
        )
        var expansionCount = 0

        let visited = boundedBreadthFirst(
            roots: [root],
            maximumDepth: 2,
            maximumNodes: 4
        ) { node in
            expansionCount += 1
            return node.children
        }

        XCTAssertEqual(visited.map(\.name), ["window", "branch-0", "branch-1", "branch-2"])
        XCTAssertEqual(expansionCount, 1)
    }

    func testChatListSelectionExcludesUnselectedMessageHistoryTable() {
        XCTAssertEqual(
            chatListContainerIndices(candidates: [
                RowContainerCandidate(selectedRowCount: 1, selectedRowsSettable: true),
                RowContainerCandidate(selectedRowCount: 0, selectedRowsSettable: true),
                RowContainerCandidate(selectedRowCount: 0, selectedRowsSettable: false),
            ]),
            [0]
        )
    }
}
