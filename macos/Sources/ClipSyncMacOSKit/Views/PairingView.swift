import AppKit
import CoreImage.CIFilterBuiltins
import SwiftUI

public struct PairingView: View {
    @ObservedObject private var model: AppModel
    @State private var pendingAction: PairingManagementAction?

    public init(model: AppModel) {
        self.model = model
    }

    public var body: some View {
        VStack(spacing: 18) {
            if model.isPaired {
                pairedDevice
            } else {
                switch model.connectionState {
                case .waitingForPeer:
                    qrOffer
                case .sasReady:
                    sasConfirmation
                case .failed:
                    PairingEmptyState(
                        title: "Pairing unavailable",
                        systemImage: "exclamationmark.triangle",
                        detail: model.lastError ?? "The relay could not complete pairing."
                    )
                default:
                    PairingEmptyState(title: "No active pairing", systemImage: "qrcode", detail: nil)
                }
            }
        }
        .padding(24)
        .frame(width: 380, height: 430)
        .alert(item: $pendingAction) { action in
            let presentation = action.presentation
            return Alert(
                title: Text(presentation.title),
                message: Text(presentation.message),
                primaryButton: .destructive(Text(presentation.confirmTitle)) {
                    action.perform(on: model)
                },
                secondaryButton: .cancel()
            )
        }
    }

    private var pairedDevice: some View {
        VStack(spacing: 18) {
            Image(systemName: "link.circle.fill")
                .font(.system(size: 48))
                .foregroundStyle(model.connectionState == .connected ? .green : .secondary)
            Text("Android device paired")
                .font(.title3.weight(.semibold))
            Label(model.statusText, systemImage: connectionSymbol)
                .foregroundStyle(.secondary)
            if model.pairingResetInProgress {
                ProgressView()
                    .controlSize(.small)
            }
            VStack(spacing: 10) {
                Button("Replace Paired Device...", systemImage: "qrcode") {
                    pendingAction = .replace
                }
                .buttonStyle(.borderedProminent)
                .disabled(model.pairingResetInProgress)

                Button(role: .destructive) {
                    pendingAction = .unpair
                } label: {
                    Label("Unpair Device...", systemImage: "link.badge.minus")
                }
                .disabled(model.pairingResetInProgress)
            }
        }
    }

    private var connectionSymbol: String {
        switch model.connectionState {
        case .connected: "checkmark.circle.fill"
        case .connecting, .reconnecting: "arrow.triangle.2.circlepath"
        case .failed, .disconnected: "exclamationmark.circle"
        default: "circle"
        }
    }

    private var qrOffer: some View {
        VStack(spacing: 16) {
            if let json = model.pairingQRJSON,
               let image = PairingQRCode.image(for: json) {
                Image(nsImage: image)
                    .interpolation(.none)
                    .resizable()
                    .frame(width: 240, height: 240)
                    .accessibilityLabel("Pairing QR code")
            } else {
                ProgressView()
                    .controlSize(.large)
                    .frame(width: 240, height: 240)
            }
            if let code = model.pairingCode {
                Text(code)
                    .font(.system(.title, design: .monospaced, weight: .semibold))
                    .textSelection(.enabled)
            }
            Text("Waiting for Android")
                .foregroundStyle(.secondary)
            Button("Cancel", role: .cancel) {
                model.cancelPairing()
            }
        }
    }

    private var sasConfirmation: some View {
        VStack(spacing: 20) {
            Image(systemName: "lock.shield")
                .font(.system(size: 44))
                .foregroundStyle(.blue)
            Text("Security code")
                .font(.headline)
            Text(model.pairingSAS ?? "------")
                .font(.system(size: 38, weight: .semibold, design: .monospaced))
                .textSelection(.enabled)
            Text("Confirm only when the same code appears on Android.")
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)
            HStack {
                Button("Cancel", role: .cancel) {
                    model.cancelPairing()
                }
                Spacer()
                Button("Codes Match") {
                    model.confirmPairing()
                }
                .buttonStyle(.borderedProminent)
                .disabled(model.pairingSAS == nil)
            }
            .frame(maxWidth: 280)
        }
    }
}

private enum PairingManagementAction: String, Identifiable {
    case unpair
    case replace

    var id: String { rawValue }

    var presentation: (title: String, message: String, confirmTitle: String) {
        switch self {
        case .unpair:
            (
                title: "Unpair Android device?",
                message: "This Mac will remove the current pairing. Clipboard history will remain on this Mac.",
                confirmTitle: "Unpair"
            )
        case .replace:
            (
                title: "Replace paired Android device?",
                message: "The current pairing will be removed before a new pairing code is created.",
                confirmTitle: "Replace Device"
            )
        }
    }

    @MainActor
    func perform(on model: AppModel) {
        switch self {
        case .unpair:
            model.unpair()
        case .replace:
            model.replacePairedDevice()
        }
    }
}

private struct PairingEmptyState: View {
    let title: String
    let systemImage: String
    let detail: String?

    var body: some View {
        VStack(spacing: 10) {
            Image(systemName: systemImage)
                .font(.system(size: 36))
                .foregroundStyle(.secondary)
            Text(title)
                .font(.headline)
            if let detail {
                Text(detail)
                    .multilineTextAlignment(.center)
                    .foregroundStyle(.secondary)
            }
        }
    }
}

private enum PairingQRCode {
    static func image(for payload: String) -> NSImage? {
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(payload.utf8)
        filter.correctionLevel = "M"
        guard let output = filter.outputImage?.transformed(by: CGAffineTransform(scaleX: 8, y: 8)) else {
            return nil
        }
        let representation = NSCIImageRep(ciImage: output)
        let image = NSImage(size: representation.size)
        image.addRepresentation(representation)
        return image
    }
}
