import ClipSyncMacOSKit
import SwiftUI

@main
struct ClipSyncMacOSApp: App {
    @StateObject private var model = AppModel()

    var body: some Scene {
        MenuBarExtra("ClipSync", systemImage: "doc.on.clipboard") {
            MenuBarContent(model: model)
        }

        Settings {
            SettingsView(settings: model.settings)
        }
    }
}
