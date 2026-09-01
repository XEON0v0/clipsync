import ClipSyncMacOSKit
import SwiftUI

@main
struct ClipSyncMacOSApp: App {
    @StateObject private var model = AppModel()

    var body: some Scene {
        MenuBarExtra("ClipSync", systemImage: "doc.on.clipboard") {
            MenuBarContent(model: model)
        }

        Window("Pair Device", id: "pairing") {
            PairingView(model: model)
        }
        .windowResizability(.contentSize)

        Window("Clipboard History", id: "history") {
            HistoryView(model: model)
        }

        Window("ClipboardSync Settings", id: "settings") {
            SettingsView(model: model)
        }
        .windowResizability(.contentSize)
    }
}
