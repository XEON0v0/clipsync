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
        static let syncPaused = "syncPaused"
        static let sensitiveRules = "sensitiveRules"
    }

    private let defaults: UserDefaults

    @Published public var serverURL: String {
        didSet {
            defaults.set(serverURL, forKey: Keys.serverURL)
        }
    }

    @Published public var syncPaused: Bool {
        didSet {
            defaults.set(syncPaused, forKey: Keys.syncPaused)
        }
    }

    @Published public var sensitiveRules: [SensitiveRule] {
        didSet {
            if let data = try? JSONEncoder().encode(sensitiveRules) {
                defaults.set(data, forKey: Keys.sensitiveRules)
            }
        }
    }

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        serverURL = defaults.string(forKey: Keys.serverURL) ?? ""
        syncPaused = defaults.bool(forKey: Keys.syncPaused)
        if let data = defaults.data(forKey: Keys.sensitiveRules),
           let decoded = try? JSONDecoder().decode([SensitiveRule].self, from: data) {
            sensitiveRules = decoded
        } else {
            sensitiveRules = []
        }
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
