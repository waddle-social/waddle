import Foundation

/// A still-image representation, best first.
public enum StillImageType: String, CaseIterable, Hashable, Sendable {
    case png = "public.png"
    case jpeg = "public.jpeg"
    case heic = "public.heic"
    case tiff = "public.tiff"

    /// The UTType identifier to load.
    public var identifier: String { rawValue }
}

/// Which representation of a pasteboard item to load as an attachment.
public enum PasteRepresentation: Hashable, Sendable {
    /// A copied file (`public.file-url`).
    case fileURL
    /// An animated GIF (`com.compuserve.gif`), kept over any still frame.
    case gif
    case image(StillImageType)

    static let fileURLType = "public.file-url"
    static let gifType = "com.compuserve.gif"
}
