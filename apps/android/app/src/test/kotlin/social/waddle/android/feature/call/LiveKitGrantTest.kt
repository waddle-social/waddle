package social.waddle.android.feature.call

import org.junit.Assert.assertEquals
import org.junit.Test
import social.waddle.client.ffi.WaddleLiveKitJoin
import java.util.Base64

/**
 * The pre-connect JWT grant validation (web engine.ts parity plus the
 * room/identity cross-check against the join credentials).
 */
class LiveKitGrantTest {
    private fun token(payload: String): String {
        val header = Base64.getUrlEncoder().withoutPadding()
            .encodeToString("""{"alg":"HS256","typ":"JWT"}""".encodeToByteArray())
        val body = Base64.getUrlEncoder().withoutPadding()
            .encodeToString(payload.encodeToByteArray())
        return "$header.$body.signature"
    }

    private fun join(token: String) = WaddleLiveKitJoin(
        url = "wss://livekit.waddle.test",
        room = "dm-room",
        identity = "alice@waddle.test/phone",
        token = token,
    )

    @Test
    fun `valid grant with matching room and identity passes`() {
        val jwt = token(
            """{"sub":"alice@waddle.test/phone","video":{"roomJoin":true,"room":"dm-room"}}""",
        )
        assertEquals(LiveKitGrantCheck.Ok, validateLiveKitGrant(join(jwt)))
    }

    @Test
    fun `token without sub claim still passes on the grant alone`() {
        val jwt = token("""{"video":{"roomJoin":true,"room":"dm-room"}}""")
        assertEquals(LiveKitGrantCheck.Ok, validateLiveKitGrant(join(jwt)))
    }

    @Test
    fun `malformed token is rejected`() {
        assertEquals(
            LiveKitGrantCheck.Invalid(LiveKitGrantDefect.MALFORMED_TOKEN),
            validateLiveKitGrant(join("not-a-jwt")),
        )
        assertEquals(
            LiveKitGrantCheck.Invalid(LiveKitGrantDefect.MALFORMED_TOKEN),
            validateLiveKitGrant(join("a.!!!!.c")),
        )
    }

    @Test
    fun `payload that is not json is rejected`() {
        val body = Base64.getUrlEncoder().withoutPadding()
            .encodeToString("plain text".encodeToByteArray())
        assertEquals(
            LiveKitGrantCheck.Invalid(LiveKitGrantDefect.MALFORMED_TOKEN),
            validateLiveKitGrant(join("h.$body.s")),
        )
    }

    @Test
    fun `missing video grant is rejected`() {
        val jwt = token("""{"sub":"alice@waddle.test/phone"}""")
        assertEquals(
            LiveKitGrantCheck.Invalid(LiveKitGrantDefect.MISSING_GRANT),
            validateLiveKitGrant(join(jwt)),
        )
    }

    @Test
    fun `roomJoin false is rejected`() {
        val jwt = token("""{"video":{"roomJoin":false,"room":"dm-room"}}""")
        assertEquals(
            LiveKitGrantCheck.Invalid(LiveKitGrantDefect.JOIN_NOT_GRANTED),
            validateLiveKitGrant(join(jwt)),
        )
    }

    @Test
    fun `blank room is rejected`() {
        val jwt = token("""{"video":{"roomJoin":true,"room":"  "}}""")
        assertEquals(
            LiveKitGrantCheck.Invalid(LiveKitGrantDefect.MISSING_ROOM),
            validateLiveKitGrant(join(jwt)),
        )
    }

    @Test
    fun `grant for a different room is rejected`() {
        val jwt = token("""{"video":{"roomJoin":true,"room":"other-room"}}""")
        assertEquals(
            LiveKitGrantCheck.Invalid(LiveKitGrantDefect.ROOM_MISMATCH),
            validateLiveKitGrant(join(jwt)),
        )
    }

    @Test
    fun `token minted for another identity is rejected`() {
        val jwt = token(
            """{"sub":"mallory@waddle.test/x","video":{"roomJoin":true,"room":"dm-room"}}""",
        )
        assertEquals(
            LiveKitGrantCheck.Invalid(LiveKitGrantDefect.IDENTITY_MISMATCH),
            validateLiveKitGrant(join(jwt)),
        )
    }
}
