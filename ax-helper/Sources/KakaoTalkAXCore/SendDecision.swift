public enum SendMethod: Equatable {
    case pressLabeledButton
    case plainReturnKey
    case refuse
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
