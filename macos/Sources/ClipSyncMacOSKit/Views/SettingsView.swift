import SwiftUI

public struct SettingsView: View {
    @ObservedObject private var model: AppModel
    @ObservedObject private var settings: SettingsStore
    @State private var validationMessage: String?

    public init(model: AppModel) {
        self.model = model
        settings = model.settings
    }

    public var body: some View {
        Form {
            Section("Relay") {
                TextField("Relay URL", text: $settings.serverURL, prompt: Text("wss://sync.example.com/ws"))
                    .textFieldStyle(.roundedBorder)
                    .onSubmit(validate)
                if let validationMessage {
                    Text(validationMessage)
                        .foregroundStyle(.red)
                }
            }
            Section("Startup") {
                Toggle(
                    "Open at Login",
                    isOn: Binding(
                        get: { model.loginItemEnabled },
                        set: { model.setLoginItemEnabled($0) }
                    )
                )
                Text(model.loginItemStatusText)
                    .foregroundStyle(model.loginItemStatus == .requiresApproval ? .orange : .secondary)
                if model.showsLoginItemSettingsLink {
                    Button("Open Login Items Settings") {
                        model.openLoginItemSettings()
                    }
                }
            }
            ExclusionRulesView(settings: settings)
            if let error = model.lastError {
                Section {
                    Text(error)
                        .foregroundStyle(.red)
                }
            }
        }
        .formStyle(.grouped)
        .frame(width: 460)
        .padding()
        .onAppear {
            model.refreshLoginItemStatus()
        }
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
