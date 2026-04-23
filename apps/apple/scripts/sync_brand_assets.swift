#!/usr/bin/env swift

import AppKit
import Foundation

private struct AppIconContents: Decodable {
    struct ImageEntry: Decodable {
        let filename: String?
        let scale: String?
        let size: String
    }

    let images: [ImageEntry]
}

private enum SyncError: Error, CustomStringConvertible {
    case missingSourceSVG(URL)
    case unreadableSVG(URL)
    case invalidIconSize(String)
    case missingIconFilenames(URL)
    case bitmapCreationFailed(Int)
    case pngEncodingFailed(Int)

    var description: String {
        switch self {
        case let .missingSourceSVG(url):
            return "Missing shared logo SVG at \(url.path)"
        case let .unreadableSVG(url):
            return "Unable to load shared logo SVG at \(url.path)"
        case let .invalidIconSize(size):
            return "Unsupported app icon size entry: \(size)"
        case let .missingIconFilenames(url):
            return "App icon catalog at \(url.path) does not list any icon filenames"
        case let .bitmapCreationFailed(size):
            return "Failed to create bitmap context for \(size)x\(size) icon"
        case let .pngEncodingFailed(size):
            return "Failed to encode \(size)x\(size) icon as PNG"
        }
    }
}

private let fileManager = FileManager.default
private let appIconBackgroundColor = NSColor(
    srgbRed: 0x12 / 255.0,
    green: 0x1F / 255.0,
    blue: 0x2B / 255.0,
    alpha: 1.0
)

private func repoRoot() -> URL {
    URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent()
        .deletingLastPathComponent()
        .deletingLastPathComponent()
        .deletingLastPathComponent()
}

private func relativePath(_ url: URL, from baseURL: URL) -> String {
    let path = url.path
    let basePath = baseURL.path.hasSuffix("/") ? baseURL.path : "\(baseURL.path)/"

    if path.hasPrefix(basePath) {
        return String(path.dropFirst(basePath.count))
    }

    return path
}

private func writeIfChanged(_ data: Data, to url: URL, updated: inout [String], baseURL: URL) throws {
    if let existing = try? Data(contentsOf: url), existing == data {
        return
    }

    try fileManager.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
    try data.write(to: url, options: .atomic)
    updated.append(relativePath(url, from: baseURL))
}

private func renderPNG(from image: NSImage, pixels: Int, backgroundColor: NSColor?) throws -> Data {
    let hasAlpha = backgroundColor == nil
    let colorSpace = CGColorSpace(name: CGColorSpace.sRGB) ?? CGColorSpaceCreateDeviceRGB()
    let bitmapInfo = hasAlpha
        ? CGImageAlphaInfo.premultipliedLast.rawValue
        : CGImageAlphaInfo.noneSkipLast.rawValue

    guard
        let cgContext = CGContext(
            data: nil,
            width: pixels,
            height: pixels,
            bitsPerComponent: 8,
            bytesPerRow: 0,
            space: colorSpace,
            bitmapInfo: bitmapInfo
        )
    else {
        throw SyncError.bitmapCreationFailed(pixels)
    }

    NSGraphicsContext.saveGraphicsState()
    defer { NSGraphicsContext.restoreGraphicsState() }

    let context = NSGraphicsContext(cgContext: cgContext, flipped: false)
    context.imageInterpolation = .high
    NSGraphicsContext.current = context

    (backgroundColor ?? .clear).setFill()
    NSRect(x: 0, y: 0, width: pixels, height: pixels).fill()

    image.draw(
        in: NSRect(x: 0, y: 0, width: pixels, height: pixels),
        from: .zero,
        operation: .sourceOver,
        fraction: 1.0
    )

    guard let cgImage = cgContext.makeImage() else {
        throw SyncError.bitmapCreationFailed(pixels)
    }

    let bitmap = NSBitmapImageRep(cgImage: cgImage)
    guard let data = bitmap.representation(using: .png, properties: [:]) else {
        throw SyncError.pngEncodingFailed(pixels)
    }

    return data
}

private func pixelDimension(for size: String, scale: String?) throws -> Int {
    let components = size.split(separator: "x")
    guard
        components.count == 2,
        let width = Double(components[0]),
        let height = Double(components[1]),
        width == height
    else {
        throw SyncError.invalidIconSize(size)
    }

    let scaleFactor: Double
    if let scale, scale.hasSuffix("x"), let parsedScale = Double(scale.dropLast()) {
        scaleFactor = parsedScale
    } else {
        scaleFactor = 1
    }

    return Int((width * scaleFactor).rounded())
}

private func syncBrandAssets() throws {
    let root = repoRoot()
    let sourceSVG = root.appendingPathComponent("chat/public/waddle-logo.svg")
    let assetsCatalog = root.appendingPathComponent("apps/apple/Waddle/Assets.xcassets", isDirectory: true)
    let appIconSet = assetsCatalog.appendingPathComponent("AppIcon.appiconset", isDirectory: true)
    let appIconContentsURL = appIconSet.appendingPathComponent("Contents.json")
    let brandImageSet = assetsCatalog.appendingPathComponent("WaddleLogo.imageset", isDirectory: true)
    let brandSVGURL = brandImageSet.appendingPathComponent("waddle-logo.svg")

    guard fileManager.fileExists(atPath: sourceSVG.path) else {
        throw SyncError.missingSourceSVG(sourceSVG)
    }

    let svgData = try Data(contentsOf: sourceSVG)
    guard let svgImage = NSImage(contentsOf: sourceSVG) else {
        throw SyncError.unreadableSVG(sourceSVG)
    }

    var updatedFiles: [String] = []

    try writeIfChanged(svgData, to: brandSVGURL, updated: &updatedFiles, baseURL: root)

    let contentsData = try Data(contentsOf: appIconContentsURL)
    let decoder = JSONDecoder()
    let contents = try decoder.decode(AppIconContents.self, from: contentsData)

    let entries = contents.images.compactMap { entry -> (String, Int)? in
        guard let filename = entry.filename else {
            return nil
        }

        let pixels = try? pixelDimension(for: entry.size, scale: entry.scale)
        return pixels.map { (filename, $0) }
    }

    guard !entries.isEmpty else {
        throw SyncError.missingIconFilenames(appIconContentsURL)
    }

    let expectedFilenames = Set(entries.map(\.0))

    let existingGeneratedIcons = try fileManager.contentsOfDirectory(
        at: appIconSet,
        includingPropertiesForKeys: nil,
        options: [.skipsHiddenFiles]
    )
    .filter { $0.pathExtension.lowercased() == "png" && !expectedFilenames.contains($0.lastPathComponent) }

    for url in existingGeneratedIcons {
        try fileManager.removeItem(at: url)
        updatedFiles.append(relativePath(url, from: root))
    }

    for (filename, pixels) in entries {
        let pngData = try renderPNG(
            from: svgImage,
            pixels: pixels,
            backgroundColor: appIconBackgroundColor
        )
        let targetURL = appIconSet.appendingPathComponent(filename)
        try writeIfChanged(pngData, to: targetURL, updated: &updatedFiles, baseURL: root)
    }

    if updatedFiles.isEmpty {
        print("Apple brand assets already up to date")
    } else {
        print("Updated Apple brand assets:")
        for path in updatedFiles.sorted() {
            print(" - \(path)")
        }
    }
}

do {
    try syncBrandAssets()
} catch let error as SyncError {
    fputs("error: \(error)\n", stderr)
    exit(1)
} catch {
    fputs("error: \(error.localizedDescription)\n", stderr)
    exit(1)
}
