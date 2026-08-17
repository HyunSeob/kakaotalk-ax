public func boundedBreadthFirst<Node>(
    roots: [Node],
    maximumDepth: Int,
    maximumNodes: Int,
    children: (Node) -> [Node]
) -> [Node] {
    guard maximumDepth >= 0, maximumNodes > 0 else { return [] }
    var queue = roots.prefix(maximumNodes).map { ($0, 0) }
    var result: [Node] = []
    var cursor = 0
    while cursor < queue.count && result.count < maximumNodes {
        let (node, depth) = queue[cursor]
        cursor += 1
        result.append(node)
        guard depth < maximumDepth else { continue }
        let remaining = maximumNodes - queue.count
        if remaining > 0 {
            queue.append(contentsOf: children(node).prefix(remaining).map { ($0, depth + 1) })
        }
    }
    return result
}

public struct RowContainerCandidate: Equatable {
    public let selectedRowCount: Int
    public let selectedRowsSettable: Bool

    public init(selectedRowCount: Int, selectedRowsSettable: Bool) {
        self.selectedRowCount = selectedRowCount
        self.selectedRowsSettable = selectedRowsSettable
    }
}

public func chatListContainerIndices(candidates: [RowContainerCandidate]) -> [Int] {
    candidates.indices.filter {
        candidates[$0].selectedRowsSettable && candidates[$0].selectedRowCount > 0
    }
}
