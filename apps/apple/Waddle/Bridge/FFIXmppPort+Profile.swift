import Foundation
import WaddleKit

extension FFIXmppPort {
    /// `knownID` lets the core skip the data IQ for an unchanged avatar
    /// (XEP-0084 §4.2). The core answers nil both for "no avatar" and for
    /// a failed lookup; an error event during the call tells them apart.
    func fetchAvatar(of jid: BareJID, knownID: String?) async -> AvatarFetch {
        guard signals.isConnected else { return .failed }
        let window = signals.beginErrorWindow(.avatar)
        let result = await client.requestAvatar(jid: jid.description, knownIds: knownID.map { [$0] } ?? [])
        let sawError = signals.endErrorWindow(window)
        return FFIInbound.avatarFetch(result, knownID: knownID, sawError: sawError)
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
