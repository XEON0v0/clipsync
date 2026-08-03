import ServiceManagement

public enum LoginItemStatus: Equatable, Sendable {
    case disabled
    case enabled
    case requiresApproval
    case unavailable
}

@MainActor
public protocol LoginItemControlling: AnyObject {
    var status: LoginItemStatus { get }
    func setEnabled(_ enabled: Bool) throws -> LoginItemStatus
    func openSystemSettings()
}

@MainActor
public final class LoginItemController: LoginItemControlling {
    public init() {}

    public var status: LoginItemStatus {
        Self.map(SMAppService.mainApp.status)
    }

    public func setEnabled(_ enabled: Bool) throws -> LoginItemStatus {
        if enabled {
            try SMAppService.mainApp.register()
        } else {
            try SMAppService.mainApp.unregister()
        }
        return status
    }

    public func openSystemSettings() {
        SMAppService.openSystemSettingsLoginItems()
    }

    private static func map(_ status: SMAppService.Status) -> LoginItemStatus {
        switch status {
        case .notRegistered:
            return .disabled
        case .enabled:
            return .enabled
        case .requiresApproval:
            return .requiresApproval
        case .notFound:
            return .unavailable
        @unknown default:
            return .unavailable
        }
    }
}
