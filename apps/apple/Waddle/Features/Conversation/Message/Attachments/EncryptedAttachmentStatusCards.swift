import SwiftUI
import WaddleKit

/// A XEP-0448 file while its ciphertext downloads and decrypts.
struct EncryptedAttachmentLoadingCard: View {
    let file: SharedFile
    let kind: MessageAttachmentKind

    var body: some View {
        MessageFileCardLabel(
            symbol: kind.symbolName,
            title: file.displayName,
            detail: "Decrypting…",
            trailingSymbol: "lock.fill"
        )
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text("\(kind.noun), \(file.displayName), decrypting"))
    }
}

/// A XEP-0448 file that could not be fetched, authenticated or decrypted.
struct EncryptedAttachmentFailedCard: View {
    let file: SharedFile
    let retry: () -> Void

    var body: some View {
        Button(action: retry) {
            MessageFileCardLabel(
                symbol: "lock.doc",
                title: file.displayName,
                detail: "Couldn't open this encrypted file",
                trailingSymbol: "arrow.clockwise"
            )
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text("Couldn't open this encrypted file, \(file.displayName)"))
        .accessibilityHint(Text("Tries again"))
    }
}
