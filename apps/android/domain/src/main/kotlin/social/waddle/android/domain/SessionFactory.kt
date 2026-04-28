package social.waddle.android.domain

import kotlinx.coroutines.CoroutineScope
import social.waddle.android.ffi.WaddleClientHandle

/**
 * Bundles the per-session repositories that all share a single
 * [WaddleClientHandle]. Construct once when the user signs in; tear down
 * with the connection on sign-out.
 */
public class WaddleSession(
    public val client: WaddleClientHandle,
    scope: CoroutineScope,
) {
    public val rooms: RoomRepository = RoomRepository(client, scope)
    public val messages: MessageRepository = MessageRepository(client, scope)
    public val presence: PresenceRepository = PresenceRepository(client, scope)
    public val avatars: AvatarRepository = AvatarRepository(client)
    public val uploads: UploadRepository = UploadRepository(client)
}
