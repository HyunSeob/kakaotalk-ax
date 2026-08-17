public struct GlobalSearchWindowCandidate: Equatable {
    public let hasChatroomsIdentifier: Bool
    public let eligibleSearchButtonCount: Int

    public init(hasChatroomsIdentifier: Bool, eligibleSearchButtonCount: Int) {
        self.hasChatroomsIdentifier = hasChatroomsIdentifier
        self.eligibleSearchButtonCount = eligibleSearchButtonCount
    }
}

public enum GlobalSearchControlDecision: Equatable {
    case unique(windowIndex: Int)
    case unavailable
}

public func globalSearchControlDecision(
    windows: [GlobalSearchWindowCandidate]
) -> GlobalSearchControlDecision {
    let chatroomWindows = windows.indices.filter { windows[$0].hasChatroomsIdentifier }
    guard chatroomWindows.count == 1,
          windows[chatroomWindows[0]].eligibleSearchButtonCount == 1 else {
        return .unavailable
    }
    return .unique(windowIndex: chatroomWindows[0])
}

public enum GlobalSearchResultDecision: Equatable {
    case unique(containerIndex: Int, rowIndex: Int)
    case missing
    case ambiguous
}

public func globalSearchResultDecision(
    containers: [[[String]]],
    expected: String
) -> GlobalSearchResultDecision {
    var matches: [(containerIndex: Int, rowIndex: Int)] = []
    for containerIndex in containers.indices {
        for rowIndex in containers[containerIndex].indices where
            containers[containerIndex][rowIndex].contains(where: {
                $0.utf8.elementsEqual(expected.utf8)
            }) {
            matches.append((containerIndex, rowIndex))
        }
    }
    switch matches.count {
    case 0:
        return .missing
    case 1:
        return .unique(
            containerIndex: matches[0].containerIndex,
            rowIndex: matches[0].rowIndex
        )
    default:
        return .ambiguous
    }
}
