/// The SDK-free media seam owned by one signed-in call store.
@MainActor public protocol CallMediaController: AnyObject {
    /// Called when an established media session dies without an XMPP terminate.
    var onFailure: (@MainActor () -> Void)? { get set }

    /// Connects, publishing the tracks `media` names. An empty ICE list means
    /// no advertisement: keep the SDK's signaling-provided servers. Throws
    /// when media can never flow. Cancellation must release any partially
    /// opened media session.
    func connect(join: CallJoin, media: CallMedia, iceServers: [IceServer]) async throws

    /// Releases the microphone, camera and SFU connection. Safe to call when not connected.
    func disconnect() async

    /// A new call is beginning. What the person chose during the last one
    /// (muted, camera off) held through any reconnect or replacement
    /// session, and must not carry into this one.
    func forgetCaptureChoices()
}
