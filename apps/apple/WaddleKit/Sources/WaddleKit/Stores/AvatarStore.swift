import Foundation
import Observation

/// XEP-0084 avatars keyed by bare JID. A fetch that found nothing is
/// remembered so rows do not refetch on every appearance.
@MainActor
@Observable
public final class AvatarStore {
    public private(set) var images: [BareJID: AvatarImage] = [:]

    @ObservationIgnored private var settled: Set<BareJID> = []
    @ObservationIgnored private var inFlight: Set<BareJID> = []

    public init() {}

    public func image(for jid: BareJID) -> AvatarImage? {
        images[jid]
    }

    /// Returns true when the caller should fetch `jid` now.
    public func beginFetchIfNeeded(_ jid: BareJID) -> Bool {
        guard !settled.contains(jid), !inFlight.contains(jid) else { return false }
        inFlight.insert(jid)
        return true
    }

    public func finishFetch(_ jid: BareJID, image: AvatarImage?) {
        inFlight.remove(jid)
        settled.insert(jid)
        images[jid] = image
    }

    /// Forces the next appearance to refetch (e.g. after publishing).
    public func invalidate(_ jid: BareJID) {
        settled.remove(jid)
    }

    public func clear() {
        images.removeAll()
        settled.removeAll()
        inFlight.removeAll()
    }
}
