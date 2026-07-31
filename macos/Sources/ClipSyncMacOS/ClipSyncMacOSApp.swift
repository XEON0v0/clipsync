import SwiftUI

@main
struct ClipSyncMacOSApp: App {
    var body: some Scene {
        MenuBarExtra("ClipSync", systemImage: "doc.on.clipboard") {
            Text("ClipSync")
        }
    }
}
