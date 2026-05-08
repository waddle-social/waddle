import Foundation
import UniformTypeIdentifiers

private func importedChatAttachment(from url: URL) -> (data: Data, fileName: String, mediaType: String)? {
    let accessed = url.startAccessingSecurityScopedResource()
    defer {
        if accessed {
            url.stopAccessingSecurityScopedResource()
        }
    }

    guard let data = try? Data(contentsOf: url) else {
        return nil
    }

    let values = try? url.resourceValues(forKeys: [.nameKey, .contentTypeKey])
    let fileName = values?.name ?? (url.lastPathComponent.isEmpty ? "attachment" : url.lastPathComponent)
    let mediaType = values?.contentType?.preferredMIMEType
        ?? UTType(filenameExtension: url.pathExtension)?.preferredMIMEType
        ?? "application/octet-stream"
    return (data, fileName, mediaType)
}

func handleImportedChatAttachments(
    _ result: Result<[URL], Error>,
    onFileSelected: ((_ data: Data, _ fileName: String, _ mediaType: String) -> Void)?
) {
    guard let onFileSelected else { return }
    guard case .success(let urls) = result else { return }
    for url in urls {
        guard let attachment = importedChatAttachment(from: url) else { continue }
        onFileSelected(attachment.data, attachment.fileName, attachment.mediaType)
    }
}
