import Foundation

public enum ClipboardHistoryContent: Equatable, Sendable {
    case text(String)
    case image
}

public enum ClipboardHistorySource: Equatable, Sendable {
    case local
    case remote
    case remoteDeferred
}

public struct ClipboardHistoryEntry: Identifiable, Equatable, Sendable {
    public let id: String
    public let tsMs: Int64
    public let content: ClipboardHistoryContent
    public let source: ClipboardHistorySource

    public init(
        id: String,
        tsMs: Int64,
        content: ClipboardHistoryContent,
        source: ClipboardHistorySource
    ) {
        self.id = id
        self.tsMs = tsMs
        self.content = content
        self.source = source
    }
}

public enum HostPairingSnapshot: Equatable, Sendable {
    case unpaired
    case offering(qrJSON: String)
    case sasReady(sas: String)
    case paired(roomID: String)
}
