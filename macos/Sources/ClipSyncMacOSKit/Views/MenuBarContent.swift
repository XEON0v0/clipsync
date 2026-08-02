import AppKit
import SwiftUI

public struct MenuBarContent: View {
    @ObservedObject private var model: AppModel

    public init(model: AppModel) {
        self.model = model
    }

    public var body: some View {
        Text(model.statusText)
        if model.connectionState == .unpaired || model.connectionState == .failed {
            Button("Pair Device", systemImage: "qrcode") {
                model.beginPairing()
            }
            .disabled(model.settings.serverURL.isEmpty)
        }
        Divider()
        Button("Settings...", systemImage: "gearshape") {
            SettingsWindowBridge.open()
        }
        Button("Quit", systemImage: "power") {
            model.shutdown {
                NSApplication.shared.terminate(nil)
            }
        }
    }
}

@MainActor
private enum SettingsWindowBridge {
    static func open() {
        NSApplication.shared.sendAction(Selector(("showSettingsWindow:")), to: nil, from: nil)
        NSApplication.shared.activate(ignoringOtherApps: true)
    }
}
