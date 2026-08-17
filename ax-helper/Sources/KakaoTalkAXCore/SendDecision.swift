public enum SendMethod: Equatable {
    case pressLabeledButton
    case plainReturnKey
    case refuse
}

public func uniqueExactMatchIndex(titles: [String?], expected: String) -> Int? {
    let matches = titles.indices.filter { index in
        guard let title = titles[index] else { return false }
        return title.utf8.elementsEqual(expected.utf8)
    }
    return matches.count == 1 ? matches[0] : nil
}

public func sendMethod(labeledButtonCount: Int) -> SendMethod {
    switch labeledButtonCount {
    case 0:
        return .plainReturnKey
    case 1:
        return .pressLabeledButton
    default:
        return .refuse
    }
}
