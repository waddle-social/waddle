package social.waddle.android.screenshots

import android.app.Application
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import social.waddle.android.client.MentionCandidate
import social.waddle.android.client.store.PresenceShow
import social.waddle.android.feature.conversation.ConversationKind
import social.waddle.android.feature.conversation.ConversationScreen
import social.waddle.android.feature.conversation.DmHeaderAvatar
import social.waddle.android.feature.conversation.DmHeaderSubline
import social.waddle.android.feature.conversation.FeedContext
import social.waddle.android.feature.conversation.HeaderHuddle
import social.waddle.android.feature.conversation.MentionPopover
import social.waddle.android.feature.conversation.MessageActionList
import social.waddle.android.feature.conversation.MessageComposer
import social.waddle.android.feature.conversation.MessageRowActions
import social.waddle.android.feature.conversation.MessageRowContext
import social.waddle.android.feature.conversation.MessageSheetActions
import social.waddle.android.feature.conversation.RoomHeaderSubline
import social.waddle.android.feature.conversation.SheetHandle
import social.waddle.android.feature.conversation.TimelineList
import social.waddle.android.feature.conversation.feedEntriesOf
import social.waddle.android.feature.search.MessageSearchTarget
import social.waddle.android.theme.Waddle
import java.util.Locale
import java.util.TimeZone

/**
 * The real conversation screen ([ConversationScreen] over a real
 * [social.waddle.android.feature.conversation.ConversationViewModel] with a
 * fake transport): a busy room, an empty room, a DM with its presence
 * header, a thread; plus the message action sheet and the mention
 * popover. Locale, zone, clock, the 24-hour setting and reduced motion
 * are pinned. Re-record after an intended change:
 * `./gradlew :app:recordRoborazziDebug`.
 */
@RunWith(AndroidJUnit4::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(sdk = [35], qualifiers = "w411dp-h891dp-xxhdpi", application = Application::class)
class ConversationScreenshotTest {
    @Before
    fun pinLocale() {
        Locale.setDefault(Locale.UK)
        TimeZone.setDefault(TimeZone.getTimeZone("UTC"))
    }

    @Test
    fun roomBusy() = captureThemed("conversation_room") {
        ConversationFixtures.Pinned {
            ConversationScreen(
                title = "#general",
                viewModel = ConversationFixtures.viewModel(
                    conversationJid = ConversationFixtures.ROOM,
                    isGroupchat = true,
                    items = ConversationFixtures.roomItems(),
                    unread = 3,
                    typing = listOf("erin"),
                ),
                onBack = {},
                onOpenThread = {},
                searchTarget = MessageSearchTarget(ConversationFixtures.ROOM, isGroupchat = true),
                selfBareJid = ConversationFixtures.SELF,
                kind = ConversationKind.Room("#general"),
                titleSubline = { RoomHeaderSubline(here = 12, huddle = HeaderHuddle(participants = 3, video = false)) },
            )
        }
    }

    @Test
    fun roomEmpty() = captureThemed("conversation_room_empty") {
        ConversationFixtures.Pinned {
            ConversationScreen(
                title = "#design-crit",
                viewModel = ConversationFixtures.viewModel(
                    conversationJid = "design-crit@muc.waddle.test",
                    isGroupchat = true,
                    items = emptyList(),
                ),
                onBack = {},
                onOpenThread = {},
                searchTarget = MessageSearchTarget("design-crit@muc.waddle.test", isGroupchat = true),
                selfBareJid = ConversationFixtures.SELF,
                kind = ConversationKind.Room("#design-crit"),
                titleSubline = { RoomHeaderSubline(here = 1, huddle = null) },
            )
        }
    }

    @Test
    fun dm() = captureThemed("conversation_dm") {
        ConversationFixtures.Pinned {
            ConversationScreen(
                title = "alice",
                viewModel = ConversationFixtures.viewModel(
                    conversationJid = ConversationFixtures.PEER,
                    isGroupchat = false,
                    items = ConversationFixtures.dmItems(),
                ),
                onBack = {},
                onOpenThread = {},
                searchTarget = MessageSearchTarget(ConversationFixtures.PEER, isGroupchat = false),
                selfBareJid = ConversationFixtures.SELF,
                kind = ConversationKind.Dm("alice"),
                titleLeading = {
                    DmHeaderAvatar(jid = ConversationFixtures.PEER, name = "alice", presence = PresenceShow.AVAILABLE)
                },
                titleSubline = { DmHeaderSubline("Available") },
                onStartCall = {},
            )
        }
    }

    @Test
    fun dmEmpty() = captureThemed("conversation_dm_empty") {
        ConversationFixtures.Pinned {
            ConversationScreen(
                title = "sol",
                viewModel = ConversationFixtures.viewModel(
                    conversationJid = "sol@waddle.test",
                    isGroupchat = false,
                    items = emptyList(),
                ),
                onBack = {},
                selfBareJid = ConversationFixtures.SELF,
                kind = ConversationKind.Dm("sol"),
                titleLeading = { DmHeaderAvatar(jid = "sol@waddle.test", name = "sol", presence = PresenceShow.AWAY) },
                titleSubline = { DmHeaderSubline("Away · idle 20m") },
            )
        }
    }

    @Test
    fun thread() = captureThemed("conversation_thread") {
        ConversationFixtures.Pinned {
            ConversationScreen(
                title = "Thread",
                viewModel = ConversationFixtures.viewModel(
                    conversationJid = ConversationFixtures.ROOM,
                    isGroupchat = true,
                    items = ConversationFixtures.threadItems(),
                    threadId = ConversationFixtures.THREAD_ROOT,
                ),
                onBack = {},
                selfBareJid = ConversationFixtures.SELF,
                titleSubline = { RoomHeaderSubline(here = null, huddle = null) },
            )
        }
    }

    @Test
    @Config(qualifiers = "w411dp-h760dp-xxhdpi")
    fun messageActionSheet() = captureThemed("conversation_message_actions") {
        ConversationFixtures.Pinned {
            val colors = Waddle.colors
            Box(
                modifier = Modifier
                    .fillMaxSize()
                    .background(colors.bg),
            ) {
                Column(
                    modifier = Modifier
                        .align(Alignment.BottomCenter)
                        .fillMaxWidth()
                        .background(
                            colors.surface,
                            RoundedCornerShape(topStart = Waddle.radius.card, topEnd = Waddle.radius.card),
                        )
                        .padding(bottom = Waddle.space.md),
                ) {
                    SheetHandle()
                    MessageActionList(
                        item = ConversationFixtures.roomItems().first { it.isMine },
                        actionable = true,
                        canPin = true,
                        isPinned = false,
                        canModerate = true,
                        actions = MessageSheetActions(
                            onDismiss = {},
                            onReact = {},
                            onReply = {},
                            onReplyInThread = {},
                            onEdit = {},
                            onRetract = {},
                            onCopy = {},
                            onSetPinned = {},
                            onModerate = {},
                        ),
                    )
                }
            }
        }
    }

    @Test
    @Config(qualifiers = "w411dp-h640dp-xxhdpi")
    fun mentionPopover() = captureThemed("conversation_mention_popover") {
        ConversationFixtures.Pinned {
            Column(Modifier.fillMaxSize()) {
                TimelineList(
                    entries = feedEntriesOf(
                        // The thread root (with its chip) and today's last messages.
                        items = ConversationFixtures.threadItems() + ConversationFixtures.roomItems().takeLast(2),
                        pending = emptyList(),
                        context = FeedContext(
                            threadId = null,
                            selfBareJid = ConversationFixtures.SELF,
                            pinnedIds = emptySet(),
                            unreadAtOpen = 0,
                            zone = ConversationFixtures.CLOCK.zone,
                        ),
                    ),
                    isLoadingOlder = false,
                    reachedHistoryStart = false,
                    onTopReached = {},
                    actions = MessageRowActions(onOpenThread = {}),
                    context = MessageRowContext(selfBareJid = ConversationFixtures.SELF),
                    modifier = Modifier.weight(1f),
                )
                MentionPopover(
                    candidates = listOf(
                        MentionCandidate("everyone", "xmpp:general@muc.waddle.test?everyone", isBroadcast = true),
                        MentionCandidate("carol", "xmpp:carol@waddle.test", false, jid = "carol@waddle.test"),
                        MentionCandidate("cass", "xmpp:cass@waddle.test", false, jid = "cass@waddle.test"),
                    ),
                    onSelect = {},
                    modifier = Modifier.padding(horizontal = Waddle.space.sm),
                )
                MessageComposer(
                    onSend = { _, _ -> },
                    onAttach = {},
                    onGif = {},
                    onSticker = {},
                    placeholder = "Message #general",
                )
            }
        }
    }
}
