import Foundation
import WaddleKit

/// XEP-0357 push, with credentials registered at `push.<domain>` over
/// XEP-0050.
extension FFIXmppPort {
    var pushServiceJID: BareJID? {
        account.flatMap { BareJID(localpart: nil, domain: "push.\($0.domain)") }
    }

    /// Registers the APNs token, then enables XEP-0357 for the assigned
    /// node. If enabling fails the device registration is withdrawn: the
    /// node may already be enabled by a sibling device, and a caller
    /// holding no registration could never disable it.
    func registerPush(deviceToken: String, environment: PushEnvironment, appID: String) async -> PushRegistration? {
        guard let service = pushServiceJID?.description else { return nil }
        let registered = await client.registerPushDevice(
            pushServiceJid: service,
            appId: appID,
            environment: FFIOutbound.pushEnvironment(environment),
            credentials: .apns(deviceToken: deviceToken)
        )
        guard let registered else { return nil }
        guard await client.enablePushNotifications(pushServiceJid: service, node: registered.node) else {
            _ = await client.disablePushDevice(pushServiceJid: service, node: registered.node, deviceId: registered.deviceId)
            return nil
        }
        return PushRegistration(serviceJID: service, node: registered.node, deviceID: registered.deviceId)
    }

    /// Per-device opt-out; sibling devices on the node keep receiving.
    func disablePush(_ registration: PushRegistration) async -> Bool {
        await client.disablePushDevice(
            pushServiceJid: registration.serviceJID,
            node: registration.node,
            deviceId: registration.deviceID
        )
    }
}
