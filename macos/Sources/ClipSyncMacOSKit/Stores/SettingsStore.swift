import Foundation

public enum SettingsError: LocalizedError, Equatable {
    case invalidRelayURL

    public var errorDescription: String? {
        "Enter a valid wss:// relay URL."
    }
}

@MainActor
public final class SettingsStore: ObservableObject {
    private enum Keys {
        static let serverURL = "serverURL"
    }

    private let defaults: UserDefaults

    @Published public var serverURL: String {
        didSet {
            defaults.set(serverURL, forKey: Keys.serverURL)
        }
    }

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        serverURL = defaults.string(forKey: Keys.serverURL) ?? ""
    }

    public func validatedServerURL() throws -> URL {
        guard let components = URLComponents(string: serverURL.trimmingCharacters(in: .whitespacesAndNewlines)),
              components.scheme?.lowercased() == "wss",
              components.host?.isEmpty == false,
              components.user == nil,
              components.password == nil,
              let url = components.url
        else {
            throw SettingsError.invalidRelayURL
        }
        return url
    }
}
