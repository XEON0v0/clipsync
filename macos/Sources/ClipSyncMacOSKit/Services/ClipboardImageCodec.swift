import AppKit
import CryptoKit
import Foundation
import ImageIO
import UniformTypeIdentifiers

public enum ClipboardImageError: LocalizedError, Equatable {
    case encodedImageTooLarge
    case invalidImage
    case dimensionsTooLarge
    case rgbaAllocationTooLarge
    case pngEncodingFailed

    public var errorDescription: String? {
        switch self {
        case .encodedImageTooLarge: "Image exceeds the 10 MiB encoded limit."
        case .invalidImage: "Clipboard image could not be decoded."
        case .dimensionsTooLarge: "Image exceeds the 50 megapixel limit."
        case .rgbaAllocationTooLarge: "Image exceeds the 256 MiB decoded limit."
        case .pngEncodingFailed: "Clipboard image could not be encoded as PNG."
        }
    }
}

public enum ClipboardImageCodec {
    public static let maximumEncodedBytes = 10 * 1024 * 1024
    public static let maximumPixels = 50_000_000
    public static let maximumRGBABytes = 256 * 1024 * 1024

    public static func payload(fromEncodedImage data: Data, enforceEncodedLimit: Bool) throws -> ClipboardPayload {
        if enforceEncodedLimit, data.count > maximumEncodedBytes {
            throw ClipboardImageError.encodedImageTooLarge
        }
        let decoded = try decode(data)
        let png = try encodePNG(decoded.image)
        guard png.count <= maximumEncodedBytes else {
            throw ClipboardImageError.encodedImageTooLarge
        }
        return .image(png: png, semanticDigest: decoded.digest)
    }

    public static func tiffData(fromPNG data: Data) throws -> Data {
        guard data.count <= maximumEncodedBytes else {
            throw ClipboardImageError.encodedImageTooLarge
        }
        let decoded = try decode(data)
        let image = NSImage(cgImage: decoded.image, size: NSSize(width: decoded.image.width, height: decoded.image.height))
        guard let tiff = image.tiffRepresentation else {
            throw ClipboardImageError.invalidImage
        }
        return tiff
    }

    private static func decode(_ data: Data) throws -> (image: CGImage, digest: String) {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil),
              let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any],
              let width = (properties[kCGImagePropertyPixelWidth] as? NSNumber)?.intValue,
              let height = (properties[kCGImagePropertyPixelHeight] as? NSNumber)?.intValue,
              width > 0,
              height > 0
        else {
            throw ClipboardImageError.invalidImage
        }

        let (pixels, pixelOverflow) = width.multipliedReportingOverflow(by: height)
        guard !pixelOverflow, pixels <= maximumPixels else {
            throw ClipboardImageError.dimensionsTooLarge
        }
        let (rgbaBytes, rgbaOverflow) = pixels.multipliedReportingOverflow(by: 4)
        guard !rgbaOverflow, rgbaBytes <= maximumRGBABytes else {
            throw ClipboardImageError.rgbaAllocationTooLarge
        }
        guard let image = CGImageSourceCreateImageAtIndex(source, 0, nil) else {
            throw ClipboardImageError.invalidImage
        }

        var rgba = Data(count: rgbaBytes)
        let rendered = rgba.withUnsafeMutableBytes { buffer -> Bool in
            guard let base = buffer.baseAddress,
                  let context = CGContext(
                      data: base,
                      width: width,
                      height: height,
                      bitsPerComponent: 8,
                      bytesPerRow: width * 4,
                      space: CGColorSpaceCreateDeviceRGB(),
                      bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue | CGBitmapInfo.byteOrder32Big.rawValue
                  )
            else {
                return false
            }
            context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
            return true
        }
        guard rendered else {
            throw ClipboardImageError.invalidImage
        }

        var semanticBytes = Data()
        semanticBytes.append(contentsOf: withUnsafeBytes(of: UInt64(width).bigEndian, Array.init))
        semanticBytes.append(contentsOf: withUnsafeBytes(of: UInt64(height).bigEndian, Array.init))
        semanticBytes.append(rgba)
        let digest = SHA256.hash(data: semanticBytes).map { String(format: "%02x", $0) }.joined()
        return (image, digest)
    }

    private static func encodePNG(_ image: CGImage) throws -> Data {
        let output = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(
            output,
            UTType.png.identifier as CFString,
            1,
            nil
        ) else {
            throw ClipboardImageError.pngEncodingFailed
        }
        CGImageDestinationAddImage(destination, image, nil)
        guard CGImageDestinationFinalize(destination) else {
            throw ClipboardImageError.pngEncodingFailed
        }
        return output as Data
    }
}
