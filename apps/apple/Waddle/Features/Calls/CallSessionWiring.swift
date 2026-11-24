import SwiftUI
import WaddleKit

/// What a call surface asks of the session, bound to the call it was drawn
/// for: a slot that has since moved to another call ignores a tap meant for
/// the last one. The dock and the stage act through the same commands.
@MainActor
struct CallCommands {
    private let calls: CallStore
    private let media: (any CallMediaControls)?
    private let sid: String?

    init(calls: CallStore, media: (any CallMediaControls)?) {
        self.calls = calls
        self.media = media
        sid = calls.state.sid
    }

    var accept: @MainActor () -> Void { onThisCall { [calls] in _ = await calls.acceptIncoming() } }
    var decline: @MainActor () -> Void { onThisCall { [calls] in _ = await calls.declineIncoming() } }
    var hangUp: @MainActor () -> Void { onThisCall { [calls] in await calls.hangUp() } }
    var dismiss: @MainActor () -> Void { onThisCall { [calls] in calls.dismiss() } }

    /// A device choice belongs to a session in progress; a tap that lands
    /// as the call ends must not become the next call's choice.
    var toggleMicrophone: @MainActor () -> Void {
        onThisCall { [calls, media] in
            guard let media, calls.state.isEstablished else { return }
            await media.setMicrophone(!media.isMicrophoneOn)
        }
    }

    /// As `toggleMicrophone`.
    var toggleCamera: @MainActor () -> Void {
        onThisCall { [calls, media] in
            guard let media, calls.state.isEstablished else { return }
            await media.setCamera(!media.isCameraOn)
        }
    }

    var toggleHand: @MainActor () -> Void {
        onThisCall { [calls] in
            guard case .inGroup = calls.state else { return }
            _ = await calls.setHandRaised(!calls.isHandRaised)
        }
    }

    private func onThisCall(_ action: @escaping @MainActor () async -> Void) -> @MainActor () -> Void {
        { [calls, sid] in
            guard calls.state.sid == sid else { return }
            Task {
                guard calls.state.sid == sid else { return }
                await action()
            }
        }
    }
}

extension CallMediaControls {
    /// What to tell the person when Waddle may not use a device their call needs.
    /// What this call's media says about our own devices.
    var mediaReport: CallMediaIssues.Report {
        CallMediaIssues.Report(
            isMicrophoneBlocked: isMicrophoneBlocked, isCameraBlocked: isCameraBlocked,
            isScreenBlocked: isScreenShareBlocked, didScreenShareFail: didScreenShareFail
        )
    }

    /// One line for a surface with no room for the notices themselves: the dock and the sidebar's card.
    var deviceNotice: String? { CallMediaIssues.line(for: mediaReport) }

    /// For the dock, which has no control over a share: a share goes on
    /// while the call is away from its stage, and the person must not forget it.
    var dockNotice: String? {
        deviceNotice ?? (isScreenShareOn ? "You’re sharing your screen. Open the call’s conversation to stop sharing." : nil)
    }
}

extension SessionCoordinator {
    /// Who a room's call surface lists (`GroupCallRoster`). The SFU's list is
    /// the truth only while its media is up; until then the room's presence is.
    func callRoster(of stage: GroupCallStage, media: (any CallMediaControls)?, speaking: Set<BareJID>) -> [GroupCallRosterRow] {
        let live = stage.hasMedia ? (media?.participants ?? []).compactMap { JID(parsing: $0.identity) } : []
        return GroupCallRoster.rows(
            presence: calls.groupPresence.participants[stage.room] ?? [], live: live, speaking: speaking, own: account.jid
        )
    }
}

/// Where call surfaces are being drawn (`CallSurface.Platform`), read from
/// the environment in one place so the dock and the stage never disagree
/// about who has the call.
@propertyWrapper
struct CallPlatform: DynamicProperty {
    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var horizontal
    @Environment(\.verticalSizeClass) private var vertical
    #endif

    @MainActor var wrappedValue: CallSurface.Platform {
        #if os(iOS)
        CallSurface.Platform(
            isPhone: UIDevice.current.userInterfaceIdiom == .phone,
            isCompactWidth: horizontal == .compact, isCompactHeight: vertical == .compact
        )
        #else
        .regularWidth
        #endif
    }
}
