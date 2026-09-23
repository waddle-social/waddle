import Foundation
import WaddleKit

/// One-line previews of a conversation's newest row.
enum RowPreview {
    /// Body text, else a word for the attachment; rooms prefix the author.
    static func text(for item: TimelineItem) -> String? {
        guard let content = content(of: item) else { return nil }
        guard item.conversation.isRoom, !item.authorName.isEmpty else { return content }
        return "\(item.authorName): \(content)"
    }

    static func content(of item: TimelineItem) -> String? {
        guard item.tombstone == nil else { return nil }
        let text = item.body.trimmingCharacters(in: .whitespacesAndNewlines)
        let files = item.message.sharedFiles
        if !text.isEmpty, !files.contains(where: { $0.url.absoluteString == text }) {
            return collapsed(text)
        }
        guard let file = files.first else { return text.isEmpty ? nil : collapsed(text) }
        if file.isImage { return "Photo" }
        if file.isVideo { return "Video" }
        if file.isAudio { return "Audio" }
        return file.displayName
    }

    /// Newlines become spaces so a single-line row does not cut mid-word.
    private static func collapsed(_ text: String) -> String {
        text.split(whereSeparator: \.isNewline).joined(separator: " ")
    }
}

/// VoiceOver text for a conversation row.
enum RowAccessibility {
    static func label(title: String, unread: Int, isMention: Bool, isMuted: Bool) -> String {
        var parts = [title]
        if unread > 0 { parts.append(unread == 1 ? "1 unread message" : "\(unread) unread messages") }
        if isMention { parts.append("mentions you") }
        if isMuted { parts.append("muted") }
        return parts.joined(separator: ", ")
    }
}

/// The second line under the signed-in user in the sidebar.
enum AccountStatusLine {
    static func text(connection: ConnectionStatus, availability: Availability, statusText: String?) -> String {
        switch connection {
        case .connecting: return "Connecting…"
        case .offline, .signedOut, .authenticationFailed: return "Offline"
        case .online:
            if let statusText, !statusText.isEmpty { return statusText }
            return AvailabilityTitle.title(availability)
        }
    }

    /// The dot shows offline while the session is not connected.
    static func availability(connection: ConnectionStatus, chosen: Availability) -> Availability {
        connection == .online ? chosen : .offline
    }
}
