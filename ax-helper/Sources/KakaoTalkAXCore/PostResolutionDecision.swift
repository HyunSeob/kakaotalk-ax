public enum PostResolutionRowSource: Equatable {
    case currentChatList
    case globalSearch
}

public enum RowFocusDecision: Equatable {
    case focusRow
    case useSelectedRow
    case refuseSelection
}

public func rowFocusDecision(
    source: PostResolutionRowSource,
    selectionReadbackVerified: Bool
) -> RowFocusDecision {
    switch source {
    case .currentChatList:
        return .focusRow
    case .globalSearch:
        return selectionReadbackVerified ? .useSelectedRow : .refuseSelection
    }
}

public enum ConversationWindowDecision: Equatable {
    case unique
    case missing
    case ambiguous
}

public func conversationWindowDecision(candidateCount: Int) -> ConversationWindowDecision {
    switch candidateCount {
    case 0: return .missing
    case 1: return .unique
    default: return .ambiguous
    }
}

public enum RowOpenMethod: Equatable {
    case pidTargetedReturn
    case pidTargetedDoubleClick
    case pressVerifiedRow
}

public func rowOpenMethod(source: PostResolutionRowSource) -> RowOpenMethod {
    source == .globalSearch ? .pidTargetedDoubleClick : .pidTargetedReturn
}

public func postResolutionFailureStatus(
    selected: Bool,
    focused: Bool,
    eventPosted: Bool,
    selectionReadbackVerified: Bool
) -> String? {
    guard selected else { return "target_row_selection_unavailable" }
    guard focused else { return "target_row_focus_unavailable" }
    guard eventPosted else { return "target_row_event_unavailable" }
    guard selectionReadbackVerified else {
        return "target_row_selection_readback_unavailable"
    }
    return nil
}
