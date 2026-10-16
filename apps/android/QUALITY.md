# Android quality matrix

Feature parity with the web client is no longer the bar; a feature counts
when it keeps working under network churn and does not regress visually.
Each row says what guards it today. Update the row in the same PR that
changes the feature.

| Column | Meaning |
| --- | --- |
| Web | Parity with `chat/`: ✅ full, 🟡 partial, ❌ missing, 🚫 out of scope |
| Churn test | A JVM test drives the real `ConnectionLoop`/session against a fake transport through drops, handoffs, half-open sockets or released demand |
| Screenshot | Roborazzi golden in `app/src/test/screenshots` (light and dark), verified in CI |
| On device | Last verified by hand on hardware; `—` means not yet |

Definition of done for an Android PR: a Roborazzi diff for UI changes, a
churn test for session changes, and an on-device check before release.

## Connection and delivery

| Area | Web | Churn test | Screenshot | On device |
| --- | --- | --- | --- | --- |
| Reconnect forever with jittered backoff | ✅ | `ConnectionLivenessTest` | — | — |
| Liveness probes (foreground, handoff, send failure, every 180 s) | ✅ | `ConnectionLivenessTest` | — | — |
| Demand-gated connection (push mode releases the stream) | n/a | `ConnectionDemandTest` (core and app) | — | — |
| Never asks the server to hold a session (XEP-0198 resume off) | n/a | `ConnectionDemandTest` | — | — |
| Outbox replays first after reconnect, one pass per row | ✅ | `XmppSessionManagerCatchupTest` | — | — |
| Bounded parallel rejoin and MAM catch-up | ✅ | `XmppSessionManagerCatchupTest` | — | — |
| History ingested once (MAM pages only) | ✅ | `TimelinePublishTest` | — | — |
| Push over UnifiedPush (XEP-0357, Web Push provider) | ✅ | `PushRegistrarTest`, `PushMessageHandlerTest` | `settings` | — |
| Push over FCM in the Play build (XEP-0357, FCM provider; RFC 8291 ciphertext only), one transport and one device at a time | n/a | `PushCoordinatorTest`, `FcmEnvelopeKeysTest`, `PushRegistrarTest`, `PushStateStoreTest` | — | — |
| Stay-connected foreground service (FOSS build; no distributor, or by choice) | n/a | — | `settings` | — |
| Inline reply and mark-read hold the stream until delivered | n/a | — | — | — |
| Own presence (ADR-010): available in the foreground or a live call; held in the background, away after 10 min and xa after 30 min with XEP-0319 idle, on a wake alarm that survives deep sleep; a manual status from another device overrides it; nothing for notification actions | 🟡 | `XmppSessionManagerPresenceTest`, `XmppSessionManagerStatusPreferenceTest`, `ConnectionDemandTest` (app) | — | — |
| A drop mid-pipeline ends the attempt at once; the next stream restarts the pipeline | n/a | `ConnectionLoopReadyWorkTest` | — | — |
| Push and the live stream (the XEP-0160 flush included) announce each message once, by XEP-0359 stanza-id | n/a | `AnnouncedItemsTest` | — | — |

Push mode limits, by design: incoming calls ring only while the app is
open (the server has no call push class), and DM pushes wait while any
other session of the account is online.

## Messaging

| Area | Web | Churn test | Screenshot | On device |
| --- | --- | --- | --- | --- |
| Channels, DMs, group DMs | ✅ | `XmppSessionManagerTest` | `timeline`, `dms`, `home` | — |
| Grouped rows, day separators, unread divider, mentions | ✅ | `FeedEntriesTest` | `timeline` | — |
| Replies, threads, reactions, corrections, retraction | ✅ | `XmppSessionManagerVerbsTest` | `timeline`, `message_actions` | — |
| Composer (formatting, mentions, slash commands, attachments) | ✅ | — | `composer` | — |
| Read markers and sync (0333, 0490) | ✅ | `XmppSessionManagerReadStateTest` | — | — |
| Search (MAM full text) | ✅ | — | — | — |
| Link previews, stickers, GIFs, encrypted files (0448) | ✅ | — | `timeline` | — |
| OMEMO | ❌ | — | — | — |

## Shell and surfaces

| Area | Web | Churn test | Screenshot | On device |
| --- | --- | --- | --- | --- |
| Tabs: Home, DMs, Activity, You | ✅ | — | `home`, `dms` | — |
| List-detail panes on tablets and foldables | n/a | `ListDetailNavigationTest` | — | — |
| Sign-in (device authorization) | ✅ | `SessionBootstrapTest` | `login` | — |
| Settings, profile, members | ✅ | — | `settings`, `members` | — |
| Calls (1:1 and Muji) | ✅ | — | — | — |
| Community: feed (0472), stories (0501), events | ✅ | ❌ | — | — |

Resume community surfaces only once the rows above are verified on device.
