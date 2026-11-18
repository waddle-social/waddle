#if os(iOS)
import Foundation
import PushKit
import WaddleKit

/// The VoIP token and wake handler live for the process, including launches
/// without a scene. The main queue reports each push before doing any work
/// that could suspend.
@MainActor
final class VoIPPushRegistry: NSObject {
    private let registry = PKPushRegistry(queue: .main)
    private let onToken: @MainActor (Data) -> Void
    private let onInvalidated: @MainActor () -> Void

    var token: Data? { registry.pushToken(for: .voIP) }

    init(onToken: @escaping @MainActor (Data) -> Void, onInvalidated: @escaping @MainActor () -> Void) {
        self.onToken = onToken
        self.onInvalidated = onInvalidated
        super.init()
        registry.delegate = self
        registry.desiredPushTypes = [.voIP]
    }

    func setEnabled(_ enabled: Bool) {
        registry.desiredPushTypes = enabled ? [.voIP] : []
    }
}

extension VoIPPushRegistry: PKPushRegistryDelegate {
    nonisolated func pushRegistry(_ registry: PKPushRegistry, didUpdate pushCredentials: PKPushCredentials, for type: PKPushType) {
        MainActor.assumeIsolated {
            guard type == .voIP else { return }
            onToken(pushCredentials.token)
        }
    }

    nonisolated func pushRegistry(_ registry: PKPushRegistry, didInvalidatePushTokenFor type: PKPushType) {
        MainActor.assumeIsolated {
            guard type == .voIP else { return }
            onInvalidated()
        }
    }

    nonisolated func pushRegistry(
        _ registry: PKPushRegistry, didReceiveIncomingPushWith payload: PKPushPayload,
        for type: PKPushType, completion: @escaping () -> Void
    ) {
        MainActor.assumeIsolated {
            let push = CallPush(userInfo: payload.dictionaryPayload)
            let name = push.map { DirectoryStore().title(for: .direct($0.conversation)) }
                ?? NSLocalizedString("Incoming voice call", comment: "")
            AppState.shared.receiveCallPush(push, callerName: name, completion: { completion() })
        }
    }
}
#endif
