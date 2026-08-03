import SwiftUI

public struct HistoryView: View {
    @ObservedObject private var model: AppModel

    public init(model: AppModel) {
        self.model = model
    }

    public var body: some View {
        Group {
            if model.history.isEmpty {
                HistoryEmptyState(
                    detail: model.historyError
                )
            } else {
                List(model.history) { entry in
                    Button {
                        model.applyHistory(entry)
                    } label: {
                        HistoryRow(entry: entry)
                    }
                    .buttonStyle(.plain)
                }
            }
        }
        .frame(minWidth: 460, minHeight: 420)
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button(role: .destructive) {
                    model.clearHistory()
                } label: {
                    Label("Clear History", systemImage: "trash")
                }
                .disabled(model.history.isEmpty)
            }
        }
        .onAppear {
            model.refreshHistory()
        }
    }
}

private struct HistoryEmptyState: View {
    let detail: String?

    var body: some View {
        VStack(spacing: 10) {
            Image(systemName: "clock.arrow.circlepath")
                .font(.system(size: 36))
                .foregroundStyle(.secondary)
            Text("No clipboard history")
                .font(.headline)
            if let detail {
                Text(detail)
                    .multilineTextAlignment(.center)
                    .foregroundStyle(.secondary)
            }
        }
    }
}

private struct HistoryRow: View {
    let entry: ClipboardHistoryEntry

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: entry.content.isImage ? "photo" : "text.alignleft")
                .frame(width: 22)
                .foregroundStyle(.secondary)
            VStack(alignment: .leading, spacing: 4) {
                Text(entry.content.summary)
                    .lineLimit(2)
                    .foregroundStyle(.primary)
                HStack(spacing: 8) {
                    Text(entry.source.label)
                    Text(entry.date, style: .relative)
                }
                .font(.caption)
                .foregroundStyle(.secondary)
                if entry.source == .remoteDeferred {
                    Label("离线收到，点击应用", systemImage: "tray.and.arrow.down")
                        .font(.caption)
                        .foregroundStyle(.orange)
                }
            }
            Spacer(minLength: 8)
            Image(systemName: "doc.on.clipboard")
                .foregroundStyle(.tertiary)
        }
        .contentShape(Rectangle())
        .padding(.vertical, 5)
    }
}

private extension ClipboardHistoryContent {
    var isImage: Bool {
        if case .image = self { return true }
        return false
    }

    var summary: String {
        switch self {
        case let .text(text): text
        case .image: "Image"
        }
    }
}

private extension ClipboardHistorySource {
    var label: String {
        switch self {
        case .local: "This Mac"
        case .remote, .remoteDeferred: "Android"
        }
    }
}

private extension ClipboardHistoryEntry {
    var date: Date {
        Date(timeIntervalSince1970: TimeInterval(tsMs) / 1_000)
    }
}
