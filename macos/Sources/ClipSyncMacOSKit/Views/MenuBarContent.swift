import AppKit
import SwiftUI

public struct MenuBarContent: View {
    @ObservedObject private var model: AppModel
    @Environment(\.openWindow) private var openWindow

    public init(model: AppModel) {
        self.model = model
    }

    public var body: some View {
        Label(model.statusText, systemImage: statusSymbol)
        if model.isPaired {
            Button("Manage Pairing...", systemImage: "link") {
                Self.activateApp()
                openWindow(id: "pairing")
            }
        } else if model.connectionState == .unpaired || model.connectionState == .failed {
            Button("Pair Device", systemImage: "qrcode") {
                model.beginPairing()
                openWindow(id: "pairing")
            }
            .disabled(model.settings.serverURL.isEmpty)
        } else if model.connectionState == .waitingForPeer || model.connectionState == .sasReady {
            Button("Show Pairing", systemImage: "qrcode") {
                openWindow(id: "pairing")
            }
        }
        Divider()
        Button("History", systemImage: "clock.arrow.circlepath") {
            model.refreshHistory()
            openWindow(id: "history")
        }
        Button("Settings...", systemImage: "gearshape") {
            SettingsWindowBridge.open()
        }
        Button("Quit", systemImage: "power") {
            model.shutdown {
                NSApplication.shared.terminate(nil)
            }
        }
    }

    /// Brings the agent app's windows to the front before opening a window.
    /// Menu-bar menu selections do not reliably activate an `LSUIElement` app.
    private static func activateApp() {
        NSApplication.shared.activate(ignoringOtherApps: true)
    }

    private var statusSymbol: String {
        switch model.connectionState {
        case .connected: "checkmark.circle.fill"
        case .connecting, .reconnecting, .waitingForPeer: "arrow.triangle.2.circlepath"
        case .sasReady: "lock.shield"
        case .failed, .disconnected: "exclamationmark.circle"
        default: "circle"
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
