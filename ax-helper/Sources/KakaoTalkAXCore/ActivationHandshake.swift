import Foundation

public enum ActivationPollDecision: Equatable {
    case verified
    case retry
    case activationTimeout
}

public enum FocusAttemptDecision: Equatable {
    case verified
    case retry
    case focusTimeout
}

public struct ActivationHandshakePolicy: Equatable {
    public static let `default` = ActivationHandshakePolicy(
        activationTimeout: 1.5,
        retryInterval: 0.05,
        maximumFocusAttempts: 3
    )

    public let activationTimeout: TimeInterval
    public let retryInterval: TimeInterval
    public let maximumFocusAttempts: Int

    public init(
        activationTimeout: TimeInterval,
        retryInterval: TimeInterval,
        maximumFocusAttempts: Int
    ) {
        self.activationTimeout = activationTimeout
        self.retryInterval = retryInterval
        self.maximumFocusAttempts = maximumFocusAttempts
    }

    public func activationDecision(
        isExpectedApplicationFrontmost: Bool,
        elapsed: TimeInterval
    ) -> ActivationPollDecision {
        if isExpectedApplicationFrontmost {
            return .verified
        }
        return elapsed < activationTimeout ? .retry : .activationTimeout
    }

    public func focusDecision(
        axOperationReportedSuccess: Bool,
        exactReadbackVerified: Bool,
        completedAttempts: Int
    ) -> FocusAttemptDecision {
        _ = axOperationReportedSuccess
        if exactReadbackVerified {
            return .verified
        }
        return completedAttempts < maximumFocusAttempts ? .retry : .focusTimeout
    }
}
