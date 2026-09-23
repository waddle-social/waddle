import Foundation
import WaddleKit

extension FFIXmppPort {
    /// No cached item ids are passed, so an advertised avatar always
    /// carries its bytes. Dimensions are unknown on fetch.
    func fetchAvatar(of jid: BareJID) async -> AvatarImage? {
        let result = await client.requestAvatar(jid: jid.description, knownIds: [])
        return result?.avatar.flatMap(FFIInbound.avatarImage)
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
