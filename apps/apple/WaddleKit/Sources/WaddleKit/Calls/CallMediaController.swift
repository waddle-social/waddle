/// The SDK-free media seam owned by one signed-in call store.
@MainActor public protocol CallMediaController: AnyObject {
    /// Called when an established media session dies without an XMPP terminate.
    var onFailure: (@MainActor () -> Void)? { get set }

    /// Validates the credentials and connects, publishing the tracks `media`
    /// names. Throws when media can never flow. Cancellation must release any
    /// partially opened media session.
    func connect(join: CallJoin, media: CallMedia) async throws

    func disconnect() async
}
