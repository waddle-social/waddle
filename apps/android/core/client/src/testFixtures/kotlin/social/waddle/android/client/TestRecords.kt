package social.waddle.android.client

import social.waddle.client.ffi.WaddleArchivedMessage
import social.waddle.client.ffi.WaddleAvatar
import social.waddle.client.ffi.WaddleCallEvent
import social.waddle.client.ffi.WaddleCallEventKind
import social.waddle.client.ffi.WaddleCallMedia
import social.waddle.client.ffi.WaddleCallThreadAnchor
import social.waddle.client.ffi.WaddleCallThreadEnded
import social.waddle.client.ffi.WaddleChatState
import social.waddle.client.ffi.WaddleInboxEntry
import social.waddle.client.ffi.WaddleInboxResult
import social.waddle.client.ffi.WaddleLinkPreview
import social.waddle.client.ffi.WaddleMamPage
import social.waddle.client.ffi.WaddleMarkupSpan
import social.waddle.client.ffi.WaddleMdsDisplayedEntry
import social.waddle.client.ffi.WaddleMessage
import social.waddle.client.ffi.WaddleMucAffiliation
import social.waddle.client.ffi.WaddleMucRole
import social.waddle.client.ffi.WaddleMujiPresence
import social.waddle.client.ffi.WaddlePinEvent
import social.waddle.client.ffi.WaddlePresence
import social.waddle.client.ffi.WaddlePresenceHat
import social.waddle.client.ffi.WaddleReference
import social.waddle.client.ffi.WaddleSharedFile
import social.waddle.client.ffi.WaddleSmResumeState
import social.waddle.client.ffi.WaddleTune
import social.waddle.client.ffi.WaddleVCard4

/** Fixture builders for the wide FFI records: overrides via named args. */

fun testMamPage(
    messages: List<WaddleArchivedMessage> = emptyList(),
    firstId: String? = messages.firstOrNull()?.mamId,
    lastId: String? = messages.lastOrNull()?.mamId,
    isComplete: Boolean = false,
): WaddleMamPage = WaddleMamPage(
    messages = messages,
    firstId = firstId,
    lastId = lastId,
    isComplete = isComplete,
)

fun testMessage(
    id: String? = "msg-1",
    from: String? = "alice@waddle.test",
    to: String? = "me@waddle.test",
    body: String? = "hello",
    messageType: String = "chat",
    timestamp: String? = null,
    stanzaId: String? = null,
    originId: String? = null,
    isMuc: Boolean = false,
    replacesId: String? = null,
    retractsId: String? = null,
    moderationTargetId: String? = null,
    moderatedBy: String? = null,
    moderationReason: String? = null,
    reactionTargetId: String? = null,
    reactionEmojis: List<String> = emptyList(),
    chatState: WaddleChatState? = null,
    stanzaIdBy: String? = null,
    displayedMarkerRequested: Boolean = false,
    mdsDisplayed: List<WaddleMdsDisplayedEntry>? = null,
    pinEvent: WaddlePinEvent? = null,
    thread: String? = null,
    replyFallbackStart: UInt? = null,
    replyFallbackEnd: UInt? = null,
    broadcastMention: String? = null,
    mentionUris: List<String> = emptyList(),
    references: List<WaddleReference> = emptyList(),
    markupSpans: List<WaddleMarkupSpan> = emptyList(),
    linkPreviews: List<WaddleLinkPreview> = emptyList(),
    isSticker: Boolean = false,
    sharedFiles: List<WaddleSharedFile> = emptyList(),
    callThread: WaddleCallThreadAnchor? = null,
    callThreadEnded: WaddleCallThreadEnded? = null,
): WaddleMessage = WaddleMessage(
    id = id,
    from = from,
    to = to,
    body = body,
    subject = null,
    messageType = messageType,
    timestamp = timestamp,
    stanzaId = stanzaId,
    stanzaIdBy = stanzaIdBy,
    stanzaIds = emptyList(),
    originId = originId,
    replacesId = replacesId,
    retractsId = retractsId,
    retractionId = null,
    isRetracted = false,
    moderationTargetId = moderationTargetId,
    moderatedBy = moderatedBy,
    moderationReason = moderationReason,
    reactionTargetId = reactionTargetId,
    reactionEmojis = reactionEmojis,
    chatState = chatState,
    displayedMarkerRequested = displayedMarkerRequested,
    displayedMarkerId = null,
    isMuc = isMuc,
    thread = thread,
    parentThreadId = null,
    markupSpans = markupSpans,
    broadcastMention = broadcastMention,
    mentionUris = mentionUris,
    references = references,
    forumPostKind = null,
    forumTitle = null,
    isSticker = isSticker,
    linkPreviews = linkPreviews,
    pinEvent = pinEvent,
    callThreadEnded = callThreadEnded,
    carbon = null,
    replyToId = null,
    replyToSender = null,
    replyFallbackStart = replyFallbackStart,
    replyFallbackEnd = replyFallbackEnd,
    callThread = callThread,
    sharedFiles = sharedFiles,
    mdsDisplayed = mdsDisplayed,
)

fun testArchivedMessage(
    mamId: String = "mam-1",
    id: String? = "msg-1",
    stanzaId: String? = null,
    originId: String? = null,
    timestamp: String? = "2026-07-15T10:00:00Z",
    from: String? = "alice@waddle.test",
    to: String? = "me@waddle.test",
    messageType: String = "chat",
    body: String? = "hello",
    isRetracted: Boolean = false,
    replacesId: String? = null,
    retractsId: String? = null,
    moderationTargetId: String? = null,
    moderatedBy: String? = null,
    moderationReason: String? = null,
    reactionTargetId: String? = null,
    reactionEmojis: List<String> = emptyList(),
    stanzaIdBy: String? = null,
): WaddleArchivedMessage = WaddleArchivedMessage(
    mamId = mamId,
    queryId = null,
    id = id,
    stanzaId = stanzaId,
    stanzaIdBy = stanzaIdBy,
    stanzaIds = emptyList(),
    originId = originId,
    timestamp = timestamp,
    from = from,
    to = to,
    messageType = messageType,
    body = body,
    subject = null,
    replacesId = replacesId,
    retractsId = retractsId,
    retractionId = null,
    isRetracted = isRetracted,
    moderationTargetId = moderationTargetId,
    moderatedBy = moderatedBy,
    moderationReason = moderationReason,
    reactionTargetId = reactionTargetId,
    reactionEmojis = reactionEmojis,
    thread = null,
    parentThreadId = null,
    replyToId = null,
    replyToSender = null,
    replyFallbackStart = null,
    replyFallbackEnd = null,
    markupSpans = emptyList(),
    broadcastMention = null,
    mentionUris = emptyList(),
    references = emptyList(),
    forumPostKind = null,
    forumTitle = null,
    isSticker = false,
    authorRealJid = null,
    callThread = null,
    callThreadEnded = null,
    sharedFiles = emptyList(),
    linkPreviews = emptyList(),
    callEvent = null,
)

fun testPresence(
    from: String? = "room@muc.waddle.test/alice",
    presenceType: String = "available",
    mucAffiliation: WaddleMucAffiliation? = null,
    mucRole: WaddleMucRole? = null,
    mucJid: String? = null,
    show: String? = null,
    hats: List<WaddlePresenceHat> = emptyList(),
    idleSince: String? = null,
    /** `110` marks the recipient's own presence (XEP-0045 §7.2.2). */
    mucStatusCodes: List<UShort> = emptyList(),
    /** XEP-0272 Muji advertisement; `null` is the §Leaving marker. */
    muji: WaddleMujiPresence? = null,
    handRaised: Boolean = false,
    muted: Boolean = false,
): WaddlePresence = WaddlePresence(
    from = from,
    to = "me@waddle.test",
    presenceType = presenceType,
    show = show,
    status = null,
    hats = hats,
    mucAffiliation = mucAffiliation,
    mucRole = mucRole,
    mucJid = mucJid,
    mucStatusCodes = mucStatusCodes,
    vcardAvatar = null,
    idleSince = idleSince,
    errorCondition = null,
    errorType = null,
    errorText = null,
    handRaised = handRaised,
    muted = muted,
    muji = muji,
)

fun testCallEvent(
    from: String = "alice@waddle.test/phone",
    sid: String = "call-1",
): WaddleCallEvent = WaddleCallEvent(
    from = from,
    to = "me@waddle.test/waddle-android-abcdef01",
    sid = sid,
    kind = WaddleCallEventKind.Propose(WaddleCallMedia(audio = true, video = false)),
)

fun testVcard4(
    fullName: String? = "Ice Puma",
    nickname: String? = null,
    pronouns: String? = null,
    note: String? = null,
    url: String? = null,
    photoUri: String? = null,
): WaddleVCard4 = WaddleVCard4(
    fullName = fullName,
    nickname = nickname,
    pronouns = pronouns,
    note = note,
    url = url,
    photoUri = photoUri,
)

fun testTune(
    artist: String? = "The Beatles",
    title: String? = "Come Together",
    source: String? = null,
    track: String? = null,
    uri: String? = null,
    lengthSeconds: UInt? = null,
    rating: UByte? = null,
): WaddleTune = WaddleTune(
    artist = artist,
    title = title,
    source = source,
    track = track,
    uri = uri,
    lengthSeconds = lengthSeconds,
    rating = rating,
)

fun testAvatar(
    jid: String = "alice@waddle.test",
    id: String = "a1b2c3",
    mimeType: String = "image/png",
    data: ByteArray = byteArrayOf(1, 2, 3),
    url: String? = null,
): WaddleAvatar = WaddleAvatar(
    jid = jid,
    id = id,
    mimeType = mimeType,
    data = data,
    url = url,
)

fun testInboxEntry(
    partner: String = "alice@waddle.test",
    kind: String = "direct",
    lastStanzaId: String? = "s1",
    lastUpdated: Long? = 1_700_000_000L,
    unread: UInt = 1u,
    preview: String? = null,
    threadId: String? = null,
    threadTitle: String? = null,
    replyCount: UInt? = null,
    author: String? = null,
): WaddleInboxEntry = WaddleInboxEntry(
    partner = partner,
    kind = kind,
    lastStanzaId = lastStanzaId,
    lastUpdated = lastUpdated,
    unread = unread,
    preview = preview,
    threadId = threadId,
    threadTitle = threadTitle,
    replyCount = replyCount,
    author = author,
)

fun testInboxResult(
    conversations: List<WaddleInboxEntry> = emptyList(),
    total: UInt = conversations.size.toUInt(),
    unread: UInt = conversations.count { it.unread > 0u }.toUInt(),
    allUnread: UInt = conversations.sumOf { it.unread },
): WaddleInboxResult = WaddleInboxResult(
    total = total,
    unread = unread,
    allUnread = allUnread,
    conversations = conversations,
)

fun testResumeState(
    previd: String = "prev-1",
    inboundH: UInt = 5u,
    outboundH: UInt = 7u,
): WaddleSmResumeState = WaddleSmResumeState(
    previd = previd,
    inboundH = inboundH,
    outboundH = outboundH,
    maxResumeSeconds = 300u,
    queuedStanzasXml = listOf("<message/>"),
)
