package social.waddle.android.screenshots

import android.app.Application
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import social.waddle.android.LoadingScreen
import social.waddle.android.RestoreErrorScreen
import social.waddle.android.client.ConnectionState
import social.waddle.android.client.prefs.ThemeMode
import social.waddle.android.client.push.PushStatus
import social.waddle.android.client.store.MemberListStatus
import social.waddle.android.client.store.PresenceShow
import social.waddle.android.feature.admin.CommunityUsersActions
import social.waddle.android.feature.admin.CommunityUsersContent
import social.waddle.android.feature.admin.CommunityUsersUiState
import social.waddle.android.feature.dm.DmListContent
import social.waddle.android.feature.dm.DmListItem
import social.waddle.android.feature.dm.DmListRow
import social.waddle.android.feature.dm.DmLiveState
import social.waddle.android.feature.dm.ListClock
import social.waddle.android.feature.members.BotRow
import social.waddle.android.feature.members.MemberFilter
import social.waddle.android.feature.members.MemberRow
import social.waddle.android.feature.members.MembersContent
import social.waddle.android.feature.members.MembersListActions
import social.waddle.android.feature.members.MembersUiState
import social.waddle.android.feature.profile.MoodDraft
import social.waddle.android.feature.profile.MoodSectionState
import social.waddle.android.feature.profile.ProfileActions
import social.waddle.android.feature.profile.ProfileContent
import social.waddle.android.feature.profile.ProfileFeedback
import social.waddle.android.feature.profile.ProfileIdentity
import social.waddle.android.feature.profile.ProfileUiState
import social.waddle.android.feature.profile.VcardSectionState
import social.waddle.android.feature.settings.DeliveryUiState
import social.waddle.android.feature.settings.SettingsActions
import social.waddle.android.feature.settings.SettingsContent
import social.waddle.android.feature.settings.SettingsUiState
import social.waddle.android.feature.you.YouContent
import social.waddle.android.feature.you.YouIdentity
import social.waddle.android.ui.huddle.LocalReducedMotion
import social.waddle.client.ffi.WaddleAdminUserEntry
import social.waddle.client.ffi.WaddleMood
import social.waddle.client.ffi.WaddleMucAffiliation
import java.time.LocalDateTime
import java.time.LocalTime
import java.util.Locale
import java.util.TimeZone

/**
 * The screens around the conversation: DMs, You, profile, settings,
 * members, community users and app startup. Locale, zone, the list
 * clock and reduced motion are pinned so every capture is still and
 * repeatable.
 */
@RunWith(AndroidJUnit4::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(sdk = [35], qualifiers = "en-rGB-w411dp-h891dp-xxhdpi", application = Application::class)
class SurfacesScreenshotTest {
    @Before
    fun pinLocale() {
        Locale.setDefault(Locale.UK)
        TimeZone.setDefault(TimeZone.getTimeZone("UTC"))
    }

    /** [captureThemed] with every loop and pulse held still. */
    private fun captureStill(name: String, content: @Composable () -> Unit) = captureThemed(name) {
        CompositionLocalProvider(LocalReducedMotion provides true) { content() }
    }

    @Test
    fun settings() = captureStill("settings") {
        SettingsContent(
            delivery = DeliveryUiState(
                status = PushStatus.ACTIVE,
                distributorLabel = "ntfy",
                distributorInstalled = true,
                stayConnected = false,
            ),
            state = SettingsUiState(
                username = "david",
                jid = "david@waddle.test",
                theme = ThemeMode.SYSTEM,
                messageSoundsEnabled = false,
            ),
            selfAvatar = null,
            isCommunityOwner = true,
            actions = SettingsActions(
                onBack = {},
                onOpenProfile = {},
                onOpenCommunityUsers = {},
                onSetTheme = {},
                onSetNotificationsEnabled = {},
                onSetMessageSoundsEnabled = {},
                onSetReadReceiptsEnabled = {},
                onLogout = {},
            ),
        )
    }

    @Test
    fun members() = captureStill("members") {
        MembersContent(
            state = MembersUiState(
                status = MemberListStatus.LOADED,
                rows = listOf(
                    member(
                        "Mara",
                        WaddleMucAffiliation.MEMBER,
                        present = true,
                        presence = PresenceShow.AVAILABLE,
                        inHuddle = true,
                    ),
                    member(
                        "David",
                        WaddleMucAffiliation.OWNER,
                        present = true,
                        presence = PresenceShow.AVAILABLE,
                        hats = listOf("Maintainer"),
                    ),
                    member("Alice", WaddleMucAffiliation.ADMIN, present = true, presence = PresenceShow.AWAY),
                    member("Bob", WaddleMucAffiliation.MEMBER, presence = PresenceShow.AVAILABLE),
                    member("Mallory", WaddleMucAffiliation.OUTCAST),
                ),
                bots = listOf(BotRow(jid = "herald@bots.waddle.test", displayName = "Herald")),
            ),
            roomName = "general",
            filter = MemberFilter.EVERYONE,
            actions = MembersListActions(),
        )
    }

    @Test
    fun dmList() = captureStill("dm_list") {
        DmListContent(
            items = listOf(
                peer("mara", "Can you hop in for five?", at(9, 41), unread = 2, live = DmLiveState.RINGING),
                peer("alice", "Pushed the Parquet compaction fix", at(8, 12), presence = PresenceShow.AVAILABLE),
                DmListItem(
                    row = DmListRow.Group(
                        roomJid = "gdm-1@muc.waddle.test",
                        name = "alice, bob, carol",
                        unreadCount = 4,
                        preview = "carol: @david dashboards are up",
                        lastUpdated = at(7, 55),
                    ),
                    live = DmLiveState.LIVE,
                    mentionCount = 1,
                    huddleCount = 2,
                ),
                peer("bob", "Sounds good, let's ship it Monday", at(18, 2, daysAgo = 1), presence = PresenceShow.AWAY),
                peer("sol", null, at(10, 30, daysAgo = 3), presence = PresenceShow.AVAILABLE, inHuddle = true),
                peer("dana", "thanks!", at(14, 5, daysAgo = 9), muted = true, unread = 3),
            ),
            onOpenDm = { _, _ -> },
            onOpenGroupDm = { _, _ -> },
            onNewGroup = {},
            clock = CLOCK,
        )
    }

    @Test
    fun dmListEmpty() = captureStill("dm_list_empty") {
        DmListContent(
            items = emptyList(),
            onOpenDm = { _, _ -> },
            onOpenGroupDm = { _, _ -> },
            onNewGroup = {},
            clock = CLOCK,
        )
    }

    @Test
    fun you() = captureStill("you") {
        YouContent(
            identity = YouIdentity(
                name = "David Flanagan",
                jid = "david@waddle.test",
                connection = ConnectionState.Ready,
                mood = WaddleMood(kind = "excited", text = "shipping Huddle on Android"),
            ),
            onOpenProfile = {},
            onOpenSettings = {},
        )
    }

    @Test
    fun profile() = captureStill("profile") {
        ProfileContent(
            state = ProfileUiState(
                vcard = VcardSectionState(
                    loading = false,
                    loaded = true,
                    fullName = "David Flanagan",
                    nickname = "rawkode",
                    pronouns = "he/him",
                    url = "https://rawkode.academy",
                    note = "Cloud native, Rust and penguins.",
                    feedback = ProfileFeedback.PUBLISHED,
                ),
                mood = MoodSectionState(draft = MoodDraft(kind = "excited", text = "shipping Huddle")),
            ),
            identity = ProfileIdentity(name = "david"),
            actions = ProfileActions(),
        )
    }

    @Test
    fun communityUsers() = captureStill("community_users") {
        CommunityUsersContent(
            state = CommunityUsersUiState(
                entries = listOf(
                    WaddleAdminUserEntry(jid = "david@waddle.test", displayName = "David", hasOwnerHat = true),
                    WaddleAdminUserEntry(jid = "alice@waddle.test", displayName = "Alice", hasOwnerHat = false),
                    WaddleAdminUserEntry(jid = "bob@waddle.test", displayName = null, hasOwnerHat = false),
                    WaddleAdminUserEntry(
                        jid = "carol.with.a.very.long.address@community.waddle.test",
                        displayName = "Carol",
                        hasOwnerHat = false,
                    ),
                ),
                nextCursor = "page-2",
            ),
            actions = CommunityUsersActions(),
        )
    }

    @Test
    fun appLoading() = captureStill("app_loading") { LoadingScreen() }

    @Test
    fun appRestoreError() = captureStill("app_restore_error") {
        RestoreErrorScreen(detail = "Connection refused (waddle.test:5222)", onRetry = {})
    }

    private fun member(
        name: String,
        affiliation: WaddleMucAffiliation,
        present: Boolean = false,
        hats: List<String> = emptyList(),
        presence: PresenceShow? = null,
        inHuddle: Boolean = false,
    ) = MemberRow(
        jid = "${name.lowercase()}@waddle.test",
        displayName = name,
        affiliation = affiliation,
        nick = name.lowercase(),
        presentNow = present,
        hats = hats,
        inferred = false,
        presence = presence,
        inHuddle = inHuddle,
    )

    private fun peer(
        name: String,
        preview: String?,
        lastUpdated: Long,
        unread: Int = 0,
        presence: PresenceShow? = null,
        inHuddle: Boolean = false,
        live: DmLiveState = DmLiveState.NONE,
        muted: Boolean = false,
    ) = DmListItem(
        row = DmListRow.Peer(
            peerJid = "$name@waddle.test",
            name = name,
            unreadCount = unread,
            isMuted = muted,
            preview = preview,
            lastUpdated = lastUpdated,
        ),
        presence = presence,
        inHuddle = inHuddle,
        live = live,
    )

    /** Epoch millis at [hour]:[minute], [daysAgo] days before the pinned today. */
    private fun at(hour: Int, minute: Int, daysAgo: Long = 0): Long =
        LocalDateTime.of(ScreenshotFixtures.TODAY.minusDays(daysAgo), LocalTime.of(hour, minute))
            .toInstant(ScreenshotFixtures.ZONE)
            .toEpochMilli()

    private companion object {
        val CLOCK = ListClock(zone = ScreenshotFixtures.ZONE, today = ScreenshotFixtures.TODAY, use24Hour = true)
    }
}
