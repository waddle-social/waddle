import CoreFoundation
import Foundation

/// A VoIP push's call invitation, before the XMPP session is connected.
public struct CallPush: Hashable, Sendable {
    public let conversation: BareJID
    public let from: JID?
    public let sid: String
    public let isVideo: Bool
    public let expires: Date
    public let node: String

    /// Parses the Push Service's versioned `waddle` object. A sender, when
    /// present, must name a resource of the conversation's bare JID.
    public init?(userInfo: [AnyHashable: Any]) {
        guard let waddle = userInfo["waddle"] as? [String: Any],
              let version = Self.number(waddle["v"]), version.doubleValue == 1,
              waddle["class"] as? String == "call",
              let address = waddle["conversation"] as? String,
              let conversation = BareJID(parsing: address),
              let item = waddle["item"] as? String, !item.isEmpty,
              let node = waddle["node"] as? String, !node.isEmpty,
              let call = waddle["call"] as? [String: Any],
              let sid = call["sid"] as? String, !sid.isEmpty,
              let video = call["video"] as? NSNumber, CFGetTypeID(video) == CFBooleanGetTypeID(),
              let expires = Self.number(call["expires"]), expires.doubleValue.isFinite
        else { return nil }

        let from: JID?
        if let sender = call["from"] {
            guard let address = sender as? String, let jid = JID(parsing: address),
                  jid.resource != nil, jid.bare == conversation
            else { return nil }
            from = jid
        } else {
            from = nil
        }
        self.conversation = conversation
        self.from = from
        self.sid = sid
        self.isVideo = video.boolValue
        self.expires = Date(timeIntervalSince1970: expires.doubleValue)
        self.node = node
    }

    /// The push deadline bounds waiting for a slot, not an adopted call.
    public func isExpired(at date: Date) -> Bool {
        expires <= date
    }

    private static func number(_ value: Any?) -> NSNumber? {
        guard let value = value as? NSNumber, CFGetTypeID(value) != CFBooleanGetTypeID() else { return nil }
        return value
    }
}
