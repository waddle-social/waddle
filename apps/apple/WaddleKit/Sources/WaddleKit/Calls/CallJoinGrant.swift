import Foundation

/// Why a join token cannot be used for the credentials it came with.
public enum CallJoinDefect: Hashable, Sendable {
    /// Not a three-part JWT with a JSON object payload.
    case malformedToken
    /// The payload has no `video` grant.
    case missingGrant
    /// `video.roomJoin` is not `true`.
    case joinNotGranted
    /// `video.room` is absent or blank.
    case missingRoom
    /// `video.room` names another room than the credentials do.
    case roomMismatch
    /// The token's `sub` is not the identity in the credentials.
    case identityMismatch
}

public extension CallJoin {
    /// Nil when the token grants joining exactly this room as this identity.
    ///
    /// A pre-flight check only: a mismatched token otherwise fails deep
    /// inside the media SDK with an opaque error. It reads the payload and
    /// verifies no signature; the media server authenticates the token.
    var grantDefect: CallJoinDefect? {
        guard let payload = tokenPayload else { return .malformedToken }
        guard let grant = payload["video"] as? [String: Any] else { return .missingGrant }
        guard grant["roomJoin"] as? Bool == true else { return .joinNotGranted }
        let granted = (grant["room"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        guard !granted.isEmpty else { return .missingRoom }
        guard granted == room else { return .roomMismatch }
        if let subject = payload["sub"] as? String, subject != identity { return .identityMismatch }
        return nil
    }

    private var tokenPayload: [String: Any]? {
        let segments = token.split(separator: ".", omittingEmptySubsequences: false)
        guard segments.count == 3 else { return nil }
        // JWT segments are unpadded base64url (RFC 7515 §2).
        var base64 = segments[1].replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        base64 += String(repeating: "=", count: (4 - base64.count % 4) % 4)
        guard let data = Data(base64Encoded: base64) else { return nil }
        return (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
    }
}
