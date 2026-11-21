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

    func publishAvatar(_ image: AvatarImage) async throws -> String {
        guard let width = UInt32(exactly: image.width), let height = UInt32(exactly: image.height) else {
            throw PortError.invalidRequest
        }
        return try await mappingPortErrors {
            try await client.publishAvatar(data: image.data, mimeType: image.mediaType, width: width, height: height)
        }
    }

    func removeAvatar() async throws {
        try await mappingPortErrors { try await client.disableAvatar() }
    }

    func fetchSharedStatus(of jid: BareJID) async throws -> SharedStatus {
        FFIInbound.sharedStatus(try await mappingPortErrors { try await client.fetchUserPepProfile(jid: jid.description) })
    }

    func publishMood(_ mood: UserMood) async throws {
        try await mappingPortErrors { try await client.publishMood(kind: mood.value, text: mood.text) }
    }

    func retractMood() async throws {
        try await mappingPortErrors { try await client.retractMood() }
    }

    func fetchOwnActivity() async throws -> UserActivity? {
        try await mappingPortErrors { try await client.fetchOwnActivity() }.map(FFIInbound.activity)
    }

    func publishActivity(_ activity: UserActivity) async throws {
        try await mappingPortErrors {
            try await client.publishActivity(general: activity.general, specific: activity.specific, text: activity.text)
        }
    }

    func retractActivity() async throws {
        try await mappingPortErrors { try await client.retractActivity() }
    }

    func publishTune(_ tune: UserTune) async throws {
        try await mappingPortErrors { try await client.publishTune(tune: FFIOutbound.tune(tune)) }
    }

    func retractTune() async throws {
        try await mappingPortErrors { try await client.retractTune() }
    }

    func fetchOwnProfileDetails() async throws -> ProfileDetails? {
        try await mappingPortErrors { try await client.fetchOwnVcard4() }.map(FFIInbound.profileDetails)
    }

    func fetchProfileDetails(of jid: BareJID) async throws -> ProfileDetails? {
        try await mappingPortErrors { try await client.fetchVcard4(jid: jid.description) }.map(FFIInbound.profileDetails)
    }

    func publishProfileDetails(_ details: ProfileDetails) async throws {
        try await mappingPortErrors { try await client.publishVcard4(vcard: FFIOutbound.vcard(details)) }
    }

    func fetchPresenceMode() async throws -> PresenceMode {
        FFIInbound.presenceMode(try await mappingPortErrors { try await client.fetchStatusPreference() })
    }

    func publishPresenceMode(_ mode: PresenceMode) async throws {
        try await mappingPortErrors {
            try await client.publishStatusPreference(preference: FFIOutbound.statusPreference(mode))
        }
    }
}
