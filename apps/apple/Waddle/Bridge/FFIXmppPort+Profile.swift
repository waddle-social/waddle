import Foundation
import WaddleKit

extension FFIXmppPort {
    /// `knownID` lets the core skip the data IQ for an unchanged avatar
    /// (XEP-0084 §4.2). The core throws for a failed lookup and answers
    /// nil only when the peer definitively has no avatar.
    func fetchAvatar(of jid: BareJID, knownID: String?) async -> AvatarFetch {
        do {
            let result = try await client.requestAvatar(jid: jid.description, knownIds: knownID.map { [$0] } ?? [])
            return FFIInbound.avatarFetch(result, knownID: knownID)
        } catch {
            BridgeLog.debug("avatar lookup failed: \(error)")
            return .failed
        }
    }

    func publishAvatar(_ image: AvatarImage) async throws {
        guard let width = UInt32(exactly: image.width), let height = UInt32(exactly: image.height) else {
            throw PortError.invalidRequest
        }
        try await mappingPortErrors {
            try await client.publishAvatar(data: image.data, mimeType: image.mediaType, width: width, height: height)
        }
    }

    func removeAvatar() async throws {
        try await mappingPortErrors { try await client.disableAvatar() }
    }

    func fetchMood(of jid: BareJID) async throws -> UserMood? {
        let profile = try await mappingPortErrors { try await client.fetchUserPepProfile(jid: jid.description) }
        return profile.mood.map(FFIInbound.mood)
    }

    func publishMood(_ mood: UserMood) async throws {
        try await mappingPortErrors { try await client.publishMood(kind: mood.value, text: mood.text) }
    }

    func retractMood() async throws {
        try await mappingPortErrors { try await client.retractMood() }
    }
}
