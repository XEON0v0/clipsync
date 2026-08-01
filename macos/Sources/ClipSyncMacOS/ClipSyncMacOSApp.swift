import ServiceManagement
import SwiftUI

@main
struct ClipSyncMacOSApp: App {
    init() {
        guard let statusPath = ProcessInfo.processInfo.environment["CLIPSYNC_SPIKE_STATUS_FILE"] else {
            return
        }
        let service = SMAppService.mainApp
        let result: String
        do {
            try service.register()
            result = "registered; status=\(String(describing: service.status))"
        } catch {
            let registrationError = error as NSError
            result = "attempted; error=\(registrationError.domain):\(registrationError.code)"
        }
        try? "SMAppService registration call completed: \(result)"
            .write(toFile: statusPath, atomically: true, encoding: .utf8)
        try? service.unregister()
    }

    var body: some Scene {
        MenuBarExtra("ClipSync", systemImage: "doc.on.clipboard") {
            Text("ClipSync")
        }
    }
}
