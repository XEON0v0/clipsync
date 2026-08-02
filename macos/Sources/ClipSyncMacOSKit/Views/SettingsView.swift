import SwiftUI

public struct SettingsView: View {
    @ObservedObject private var settings: SettingsStore
    @State private var validationMessage: String?

    public init(settings: SettingsStore) {
        self.settings = settings
    }

    public var body: some View {
        Form {
            TextField("Relay URL", text: $settings.serverURL, prompt: Text("wss://sync.example.com/ws"))
                .textFieldStyle(.roundedBorder)
                .onSubmit(validate)
            if let validationMessage {
                Text(validationMessage)
                    .foregroundStyle(.red)
            }
        }
        .formStyle(.grouped)
        .frame(width: 420)
        .padding()
    }

    private func validate() {
        do {
            _ = try settings.validatedServerURL()
            validationMessage = nil
        } catch {
            validationMessage = error.localizedDescription
        }
    }
}
