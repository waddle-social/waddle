import Foundation

// MARK: - Avatars (XEP-0084)

extension AppModel {
    /// Kick off a XEP-0084 avatar fetch for the given sender. Idempotent: if
    /// the JID is already cached, currently being fetched, or resolves to the
    /// local session, the call is a no-op. A missing/empty avatar is stored
    /// as `Data()` so we don't re-request on every scroll.
    func requestAvatarIfNeeded(forSenderID senderID: String) {
        guard !senderID.isEmpty, let session, let rustClient else { return }
        let key = avatarJID(forSenderID: senderID, session: session).lowercased()
        guard !key.isEmpty else { return }
        if avatarDataByJID[key] != nil { return }
        if inFlightAvatarFetches.contains(key) { return }
        inFlightAvatarFetches.insert(key)
        Task { [weak self] in
            let avatar = await rustClient.requestAvatar(jid: key)
            let avatarData = await Self.avatarData(from: avatar)
            guard let self else { return }
            await MainActor.run {
                self.inFlightAvatarFetches.remove(key)
                if let avatarData {
                    // Empty Data is a sentinel for users without a published
                    // avatar. Failed URL fetches are not cached so they can
                    // recover on a later request.
                    self.avatarDataByJID[key] = avatarData
                }
            }
        }
    }

    private nonisolated static func avatarData(from avatar: WaddleAvatar?) async -> Data? {
        guard let avatar else { return Data() }
        if !avatar.data.isEmpty { return avatar.data }
        guard
            let value = avatar.url,
            let url = URL(string: value),
            url.scheme == "https"
        else {
            return Data()
        }
        do {
            let (data, response) = try await URLSession.shared.data(from: url)
            guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
                return nil
            }
            if let contentType = http.value(forHTTPHeaderField: "Content-Type")?.lowercased(),
               !contentType.hasPrefix("image/") {
                return nil
            }
            return data
        } catch {
            return nil
        }
    }

    /// Resolve the bare JID we should query for an avatar from a timeline
    /// message's `senderID`. MUC occupant ids arrive as bare nicknames; turn
    /// those into `nick@domain` using the session's domain. DM/1:1 senders
    /// already arrive as bare JIDs (`localpart@domain`).
    private func avatarJID(forSenderID senderID: String, session: WaddleSession) -> String {
        if senderID.contains("@") {
            return senderID
        }
        let domain = jidDomain(session.jid)
        return "\(senderID)@\(domain)"
    }

    /// Raw avatar image data for a given message `senderID`, or nil when the
    /// fetch hasn't completed or the user has no avatar. Intended for use by
    /// SwiftUI row renderers alongside an initials fallback.
    func avatarData(forSenderID senderID: String) -> Data? {
        guard !senderID.isEmpty, let session else { return nil }
        let key = avatarJID(forSenderID: senderID, session: session).lowercased()
        guard let data = avatarDataByJID[key], !data.isEmpty else { return nil }
        return data
    }
}
