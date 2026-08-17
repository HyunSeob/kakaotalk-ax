import Foundation

public struct ProcessCandidate: Equatable {
    public let pid: Int32
    public let bundleIdentifier: String
    public let terminated: Bool

    public init(pid: Int32, bundleIdentifier: String, terminated: Bool) {
        self.pid = pid
        self.bundleIdentifier = bundleIdentifier
        self.terminated = terminated
    }
}

public enum ProcessDecision: Equatable {
    case use(pid: Int32)
    case missing
}

public func kakaoTalkProcessDecision(
    candidates: [ProcessCandidate],
    bundleIdentifier: String = "com.kakao.KakaoTalkMac"
) -> ProcessDecision {
    let matches = candidates
        .filter { !$0.terminated && $0.bundleIdentifier == bundleIdentifier }
        .sorted { $0.pid < $1.pid }
    guard let first = matches.first else { return .missing }
    return .use(pid: first.pid)
}

public struct WindowCandidate: Equatable {
    public let id: UInt32
    public let pid: Int32
    public let layer: Int
    public let isOnScreen: Bool
    public let alpha: Double
    public let width: Double
    public let height: Double

    public init(
        id: UInt32,
        pid: Int32,
        layer: Int,
        isOnScreen: Bool,
        alpha: Double,
        width: Double,
        height: Double
    ) {
        self.id = id
        self.pid = pid
        self.layer = layer
        self.isOnScreen = isOnScreen
        self.alpha = alpha
        self.width = width
        self.height = height
    }

    public var area: Double { width * height }
}

public func selectedWindow(candidates: [WindowCandidate], validatedPID: Int32) -> WindowCandidate? {
    candidates
        .filter {
            $0.pid == validatedPID
                && $0.layer == 0
                && $0.isOnScreen
                && $0.alpha > 0
                && $0.width >= 120
                && $0.height >= 120
        }
        .sorted {
            if $0.area != $1.area { return $0.area > $1.area }
            if $0.width != $1.width { return $0.width > $1.width }
            if $0.height != $1.height { return $0.height > $1.height }
            return $0.id < $1.id
        }
        .first
}

public enum ExactIdentityDecision: Equatable {
    case unique(index: Int)
    case missing
    case ambiguous
}

public func exactRowIdentityDecision(rows: [[String]], expected: String) -> ExactIdentityDecision {
    let matches = rows.indices.filter { index in
        rows[index].contains { $0.utf8.elementsEqual(expected.utf8) }
    }
    switch matches.count {
    case 0: return .missing
    case 1: return .unique(index: matches[0])
    default: return .ambiguous
    }
}

public enum CardinalityDecision: Equatable {
    case unique(index: Int)
    case missing
    case ambiguous
}

public func cardinalityDecision(count: Int) -> CardinalityDecision {
    switch count {
    case 0: return .missing
    case 1: return .unique(index: 0)
    default: return .ambiguous
    }
}

public struct BackgroundTargetPolicy: Equatable {
    public static let `default` = BackgroundTargetPolicy(
        launchActivates: false,
        requiresFrontmostApplication: false,
        processDeadline: 2.0,
        windowDeadline: 2.0,
        composerDeadline: 2.0,
        retryInterval: 0.05
    )

    public let launchActivates: Bool
    public let requiresFrontmostApplication: Bool
    public let processDeadline: TimeInterval
    public let windowDeadline: TimeInterval
    public let composerDeadline: TimeInterval
    public let retryInterval: TimeInterval

    public init(
        launchActivates: Bool,
        requiresFrontmostApplication: Bool,
        processDeadline: TimeInterval,
        windowDeadline: TimeInterval,
        composerDeadline: TimeInterval,
        retryInterval: TimeInterval
    ) {
        self.launchActivates = launchActivates
        self.requiresFrontmostApplication = requiresFrontmostApplication
        self.processDeadline = processDeadline
        self.windowDeadline = windowDeadline
        self.composerDeadline = composerDeadline
        self.retryInterval = retryInterval
    }

    public func acceptsTarget(isFrontmost: Bool) -> Bool {
        !requiresFrontmostApplication || isFrontmost
    }
}
