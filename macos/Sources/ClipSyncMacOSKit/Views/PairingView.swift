import AppKit
import CoreImage.CIFilterBuiltins
import SwiftUI

public struct PairingView: View {
    @ObservedObject private var model: AppModel

    public init(model: AppModel) {
        self.model = model
    }

    public var body: some View {
        VStack(spacing: 18) {
            switch model.connectionState {
            case .waitingForPeer:
                qrOffer
            case .sasReady:
                sasConfirmation
            case .connecting, .connected:
                Label(model.statusText, systemImage: "checkmark.circle.fill")
                    .font(.title3)
                    .foregroundStyle(.green)
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
        .padding(24)
        .frame(width: 380, height: 430)
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
