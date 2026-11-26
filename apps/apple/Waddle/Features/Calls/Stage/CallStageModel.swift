import SwiftUI
import WaddleKit

/// What the person chose on the stage of the session's call: the view, a
/// pin, their own view hidden. It belongs to the session, not to a view, so
/// it is still there when they come back to the call's conversation.
@MainActor @Observable
final class CallStageModel {
    private var memory = CallStageMemory()
    @ObservationIgnored private(set) var tileSource = CallTileSource(video: { _ in nil }, togglePin: { _ in /* no call yet */ })
    /// The share of the pane each conversation's stage was dragged to, kept
    /// on this device (web keeps it per room in local storage).
    private var splitPercents: [String: Double]
    @ObservationIgnored private let defaults: UserDefaults
    private static let splitPercentsKey = "callSplitPercents"

    /// - Parameter media: where tiles get their pictures; `nil` draws avatars only.
    init(calls: CallStore, media: (any CallMediaControls)?, defaults: UserDefaults = .standard) {
        self.defaults = defaults
        splitPercents = defaults.dictionary(forKey: Self.splitPercentsKey)?.compactMapValues { $0 as? Double } ?? [:]
        tileSource = CallTileSource(
            video: { [weak media] tile in media?.video(of: tile.identity, source: tile.source) },
            // A pin is for whatever call is on the stage when the tile is clicked.
            togglePin: { [weak self, weak calls] tile in
                guard let self, let sid = calls?.state.sid else { return }
                update(sid) { $0.togglePin(tile) }
            }
        )
    }

    /// The choices for call `sid`; a call nobody has touched has the defaults.
    func stage(of sid: String) -> CallStageMemory {
        var memory = memory
        memory.follow(call: sid)
        return memory
    }

    /// Where the slot's call shows in the window laid out as `layout`, while
    /// `selection` is its open conversation. A window with no layout, as a
    /// sheet, is taken to have room for everything.
    func surface(
        of state: CallState, open selection: ConversationID?, platform: CallSurface.Platform, in layout: CallWindowLayout?
    ) -> CallSurface {
        CallSurface.placement(
            of: state, open: selection, platform: platform, pane: layout?.pane,
            size: state.sid.map { stage(of: $0).size } ?? .defaultSize,
            isOpened: state.sid.map { stage(of: $0).isOpened } ?? false, isSidebarShown: layout?.isSidebarShown ?? true
        )
    }

    func splitPercent(for conversation: ConversationID) -> Double? {
        splitPercents[conversation.description]
    }

    func setSplitPercent(_ percent: Double, for conversation: ConversationID) {
        let percent = CallSurface.clampSplitPercent(percent)
        guard splitPercents[conversation.description] != percent else { return }
        splitPercents[conversation.description] = percent
    }

    /// Once a drag is over, not on each point of it.
    func saveSplitPercents() {
        defaults.set(splitPercents, forKey: Self.splitPercentsKey)
    }

    /// Writes only a change, so an update that changes nothing redraws nothing.
    func update(_ sid: String, _ change: (inout CallStageMemory) -> Void) {
        var next = memory
        next.follow(call: sid)
        change(&next)
        if next != memory { memory = next }
    }
}
