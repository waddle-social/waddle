# Meeting-platform feature gap matrix: Waddle vs. Zoom, Teams, Meet, Slack Huddles, Webex

Research date: 2026-07-26. Wayfinder research ticket #1491 / map issue #1489.

## 1. Intro and method

Waddle is XMPP-native. Real-time call signaling runs over Jingle (XEP-0166 Jingle,
XEP-0167 Jingle RTP Sessions) and MUJI (XEP-0272, multi-user Jingle) for group
calls, with [LiveKit](https://livekit.io) as the actual SFU media backend behind
that signaling layer, and TURN/STUN relay discovery via XEP-0215 (external service
discovery / extdisco). Chat, presence, MUC membership, and moderation all live in
the XMPP layer; LiveKit is an implementation detail of the media path, not a
parallel product surface.

Waddle is **not currently positioned as a full meeting-platform competitor**. This
document is a gap analysis against the commercial meeting-platform bar set by
Zoom, Microsoft Teams, Google Meet, and Webex (plus Slack Huddles as a
lightweight, explicitly-not-a-competitor reference point) — it is not a claim
that Waddle currently targets, or should target, full parity with those
products. Its purpose is to give the team an accurate, sourced picture of what
"table stakes" and "differentiator" features look like in that market, to
inform prioritization.

**Method**: the Waddle column is a pre-verified repo inventory (file-path cited,
not re-derived here). The five vendor columns come from primary-source lookups
(WebSearch + WebFetch) against each vendor's official documentation —
support.zoom.us / support.zoom.com, learn.microsoft.com/microsoftteams,
support.google.com (Meet, Vault, Workspace Admin), slack.com/help.slack.com, and
help.webex.com — conducted 2026-07-26. Every URL actually consulted is listed in
Section 4. Numbers not confirmable on an official page are described
qualitatively rather than guessed.

**Framing note on Slack Huddles**: Huddles is a lightweight audio/video
quick-call feature bolted onto a chat app, not a full meeting platform. Slack
does not attempt breakout rooms, webinars, waiting rooms, or recording as
first-party features, and that is by design, not an oversight. Many cells in
the Slack column are legitimately **n/a** rather than **missing** — treating
them as failures against a meeting-platform checklist would misrepresent what
Huddles is for.

## 2. Feature gap matrix

| # | Area | Waddle | Zoom | Teams | Meet | Slack Huddles | Webex |
|---|------|--------|------|-------|------|----------------|-------|
| 1 | Scheduling/calendar integration | **partial** — xCal event/RSVP exists (`server/crates/waddle-xmpp-core/src/xcal.rs`, `EventsPane`/`CalendarFeedUrlPanel.vue`) but no link from a calendar event to an auto-provisioned/joinable call | **have** — native Outlook/Google Calendar scheduler plugins, "Schedule a meeting" flows | **have** — deeply integrated into Outlook/M365 calendar; meetings, webinars, town halls all scheduled via calendar object | **have** — native Google Calendar integration, auto-generated Meet links per event | **n/a** — huddles are ad hoc, no scheduling model; chat-based, start-now only | **have** — Outlook/Google Calendar integrations, scheduler productivity tools |
| 2 | External guest join (no account) | **missing** — only authenticated-JID join path found | **have** — guests join via link, no Zoom account required | **have** — anonymous/external join via link for guests outside the tenant | **have** — external users can join via link (subject to admin-configured guest settings) | **n/a** — Huddles requires Slack workspace membership (or Slack Connect for cross-org); no anonymous public join | **have** — guests join via link without a Webex account |
| 3 | Recording (+ retention/access controls) | **missing/stub** — recording indicator in `CallStageHeader.vue` hardcoded to `false`; LiveKit Egress integration (issue #1023) filed but OPEN/undeployed pending HITL infra deploy; no retention or access-control code | **have** — cloud recording; commonly cited 180-day default before auto-delete (varies by admin config); access via host/admin sharing settings | **have** — recordings upload to organizer OneDrive/organizer's SharePoint; default expiration commonly cited as 120 days (admin-configurable 1–99,999 days or disabled); eDiscovery search via Microsoft Purview | **have** — recordings stored in Drive; retention governed by Vault retention rules (Drive-based); Vault holds override retention and can preserve indefinitely | **missing** — no built-in Huddle recording; 3rd-party tools only; any transcript/notes retention follows workspace message-retention settings | **have** — recording retention commonly cited as 1 year (recordings >360 days auto-deleted by default); Pro Pack extends retention up to 3600 days; legal-hold/compliance officer access via Control Hub |
| 4 | Transcription / live captions | **missing** — no code found | **have** — live captions in ~46 languages; cloud-recording transcripts in 12 languages; translated captions cover ~36 spoken → ~100 caption display languages | **have** — real-time captions with speaker attribution; live translated captions/transcription available, org can select several languages (more with Teams Premium, up to 10 org-selected of 50+); Premium license required on organizer side for translated captions | **have** — live captions for all users; translated captions require an eligible Workspace edition; separate live speech-translation feature (English ⇄ Spanish/French/German/Portuguese/Italian) requires paid plan + Gemini add-on | **partial** — AI-generated Huddle notes/transcript on paid plans, embedded in a canvas in the huddle thread (not fully searchable); no live on-screen captions during the huddle itself | **have** — host can present in one of 15 spoken languages, closed captions translatable into 100+ languages for participants; real-time translation/transcription in meetings and webinars |
| 5 | Breakout rooms | **missing** — no code found | **have** — standard host feature | **have** — supported in Teams meetings | **have** — host-managed breakout rooms, host-management locks applied globally or per-room | **n/a** — no breakout concept in a huddle | **have** — Webex breakout sessions in meetings |
| 6 | Webinars / large-room capacity | **missing** — no viewer-only or large-capacity room mode | **have** — webinars scale from 500 up to 1,000,000 view-only attendees (add-on license; >200,000 requires advance coordination with Zoom); large-meeting add-on separately supports 500/1,000/3,000/5,000 interactive participants | **have** — standard meetings ~1,000 interactive + view-only up to 10,000 (≈11,000 total); events optimized for large audiences (town halls) scale to 100,000 attendees, broadcast-style | **have** — meetings scale 100–1,000 interactive participants by Workspace edition; live-streaming view-only mode reaches up to 100,000 viewers | **n/a** — huddles cap at 50 participants (paid plans; free plan capped at 2), not a broadcast/webinar product | **have** — meetings up to 1,000 attendees; webinars up to 5,000 attendees; webcast view for larger webinars and events up to 100,000 viewers |
| 7 | Screen share quality (resolution/codec/optimization) | **have** — VP9+SVC (L1T3 layer profile) for screen share with VP8 simulcast fallback for non-VP9 decoders (e.g. iOS), capped at 720p (`chat/src/lib/calls/video-codec/video-publish.ts`, `support.ts`) | **have** — dynamic resolution/optimization modes (video vs. text/detail), HD options on paid tiers | **have** — content-optimized screen sharing, adaptive resolution | **have** — adaptive screen-share resolution, "Present a tab/window/screen" with codec optimization | **partial** — up to 2 participants can share screen simultaneously in a huddle; no stated codec/optimization-mode detail in help docs | **have** — screen/content share with dynamic resolution |
| 8 | Virtual backgrounds / noise suppression | **have** — blur + curated image backgrounds (WASM/WebGL, `chat/src/lib/calls/background-effect/`), WASM AI noise filter (RNNoise/DTLN, Light/Balanced/Maximum tiers, `chat/src/lib/calls/ai-noise-filter/`), plus standard echo-cancellation/AGC toggles in `CallSettingsDialog.vue` | **have** — virtual backgrounds + background noise suppression (Low/Medium/High/Automatic levels) | **have** — background blur/replace, Teams noise suppression settings | **have** — background blur/replace; Google's own noise-cancellation | **have** — video background options in huddles (basic); no dedicated noise-suppression tiering documented | **have** — virtual backgrounds; "Webex Smart Audio" noise removal with modes (Noise removal, Optimize for my voice, Optimize for all voices, Music mode) |
| 9 | Reactions / hand-raise | **have** — `in-call-reactions.ts`, `call-raised-hand.ts`, custom `urn:waddle:in-call:0` carrier, raise-hand as call presence state (issue #1029) | **have** — reaction emojis + raise-hand toolbar button | **have** — reactions + raise-hand | **have** — reactions + raise-hand | **have** — emoji reactions, effects, GIFs (temporary) and stickers incl. ✋ Raise Hand / ☕ Be Right Back (persist until removed) | **have** — reactions + raise-hand |
| 10 | Host/moderation controls | **partial** — MUC-level kick/ban/lock exists server-side (`server/crates/waddle-xmpp/src/muc/admin.rs`, `room_actor/admin_handlers.rs`) and SFU-side forced remove/delete-room exists (`server/crates/waddle-sfu/src/admin.rs`), but no dedicated in-call "mute all"/"remove from call" UI distinct from generic MUC admin | **have** — mute-all, remove participant (cannot rejoin), lock meeting, all as first-class in-meeting host tools | **have** — organizer/co-organizer controls: mute all, remove, lock meeting | **have** — host controls incl. mute all, remove, host management locks (esp. for breakout rooms) | **partial** — huddle host/creator can manage participants at a basic level; not a purpose-built moderation suite | **have** — host controls: mute all, expel participant, lock meeting |
| 11 | Waiting rooms / lobby | **missing** — no code found for calls; only unrelated chat-thread "lobby" concept | **have** — Waiting Room under Security controls, admit individually or all | **have** — lobby/"people waiting" admission control | **have** — waiting room; host can message everyone in the waiting room | **n/a** — no lobby/gating concept for huddles | **have** — waiting-room/lobby equivalent in meeting security settings |
| 12 | End-to-end encryption offerings | **missing beyond transport default** — only DTLS-SRTP (WebRTC/LiveKit default); no Insertable-Streams/per-participant frame E2EE; repo's only "E2EE" hits are XEP-0448 (file-sharing encryption), unrelated to call media | **have, with trade-offs** — optional E2EE mode; disables cloud recording, live transcription/captions, breakout rooms, polling, 1:1 private chat, streaming, join-before-host; phone/SIP/H.323 excluded; capped at 1,000 participants | **have, with trade-offs** — Teams Premium E2EE mode; disables recording, live captions, transcription, Together Mode, large meetings/gallery, PSTN; web/VDI/CVI clients blocked; capped at 200 participants | **have** — client-side encryption (CSE) with customer-controlled keys via external IdP+key service (admin setup required); separate optional E2EE for personal 1:1/group calls | **missing** — no E2EE mode documented for huddles | **have, tiered** — standard Webex E2EE (default for user-generated content) plus a stronger Zero-Trust E2EE mode for media; FIPS-validated crypto available for E2EE mode |
| 13 | Admin/compliance (audit logs, legal hold, DLP) | **missing** — no call-specific audit-log, legal-hold, or DLP code found | **have** — admin dashboards, audit-style reporting; compliance ecosystem largely via 3rd-party/partner integrations | **have** — deep Microsoft Purview integration: eDiscovery (Premium) case management, legal hold on user/team mailbox+OneDrive+Teams content, retention policies, DLP as part of the M365 compliance suite | **have** — Google Vault: retention rules for Meet-linked Drive content, legal holds (override retention), full Vault audit log of search/export/hold actions, searchable recordings+captions+chat+attendance+Q&A/poll logs | **partial** — enterprise/Enterprise Grid plans get workspace-wide message retention and some compliance exports; no huddle-specific audit/legal-hold feature documented | **have** — Control Hub compliance officer role, configurable retention (Pro Pack: up to 3,600 days), legal-hold preservation of call recordings/CDRs |
| 14 | Reliability/quality claims | **have, as internal capability, not a customer SLA** — VP9+VP8 simulcast with SVC temporal/spatial layers (L1T3/L3T3), XEP-0215 extdisco-driven TURN/STUN fallback to a LiveKit-managed TURN host with time-limited HMAC credentials, extensive internal telemetry (ICE candidate path, media path, connection quality — `call-ice-telemetry.ts`, `call-media-path-telemetry.ts`, `call-stats.ts`, `connection-quality.ts`); no published uptime SLA (not a commercial SaaS product today) | **have** — published MSA-level SLA language (commonly cited 99.9%, with a separate credit-backed 99.999% SLA specific to Zoom Phone); public historical uptime dashboard (uptime.zoom.us) and live status page | **have** — published 99.9% core-service SLA (Teams Phone/Calling Plan/Audio Conferencing raised to 99.99%); service credits on SLA miss | **have** — published Google Workspace SLA: ≥99.9% monthly uptime with tiered service credits (99.0–99.9% / 95.0–99.0% / <95.0%) | **n/a** — no huddle-specific SLA; covered only by Slack's general service-level commitments as part of the broader product | **have** — adaptive/simulcast media handling as part of the Webex media stack; enterprise SLA commitments as part of Webex Suite contracts (not itemized with a single public numeric SLA in the pages reviewed) |

## 3. Table-stakes vs. differentiators

Looking at Waddle's **missing**/**partial** rows, they split unevenly between
things a credible meeting-platform competitor is expected to have out of the
box, and things that are more selectively offered even among the four full
platforms studied.

**Table stakes** (present, in some form, across effectively all four full
platforms — Zoom/Teams/Meet/Webex — so their absence is the sharpest
credibility gap):

- **Recording** — all four have first-party cloud/local recording with
  retention and access controls (Zoom, Teams, Webex all publish concrete
  default-retention numbers; Meet ties retention to Vault rules). Waddle's
  recording path is a UI stub wired to `false` with the actual LiveKit Egress
  backend still undeployed (#1023) — this is the single largest table-stakes
  gap.
- **Live captions/transcription** — universal across Zoom, Teams, Meet, and
  Webex, each with substantial multilingual support (36–46+ languages).
  Waddle has none.
- **Host mute-all / remove-participant / lock meeting** — universal, and
  exposed as dedicated in-meeting host-tool UI on all four platforms, not
  buried in room administration. Waddle has the underlying MUC/SFU admin
  primitives but no purpose-built in-call control surface — a partial gap,
  closer to a UX/wiring problem than a missing primitive.
- **Waiting rooms/lobby** — universal (Zoom Security panel, Teams lobby,
  Meet waiting room). Waddle has no call-gating concept at all.
- **External guest join without an account** — universal (all four support
  link-based guest join). Waddle only supports authenticated-JID join, which
  is also an XMPP-architecture question, not just a feature gap.

**Differentiators** (offered by some platforms but meaningfully skipped,
gated behind premium tiers, or come with heavy feature trade-offs even among
the leaders — so absence is a lesser competitive concern):

- **Breakout rooms** — present on Zoom/Teams/Meet/Webex, but it is a
  moderation-heavy, session-management feature mostly relevant to
  training/education use cases rather than typical 1:1 or small-team calls;
  lower urgency than captions/recording for Waddle's current chat-first
  audience.
- **Webinars/large-room broadcast mode** — all four platforms support it, but
  it is explicitly a *separate licensed product tier* in every case (Zoom
  webinar add-on, Teams "events optimized for large audiences," Meet
  live-streaming, Webex Webinars/webcast) rather than baked into the core
  meeting experience — evidence this is treated industry-wide as an
  upsell/differentiator, not a baseline expectation.
- **End-to-end encryption** — offered by Zoom, Teams, Meet (CSE), and Webex,
  but every vendor ships it with severe feature trade-offs (Zoom/Teams both
  disable recording, captions, and breakout rooms in E2EE mode; Teams also
  caps participants at 200 and blocks web/VDI clients). This is a
  security-conscious-buyer feature, not something a mainstream buyer expects
  by default — reasonable to defer.
- **Admin/compliance (audit logs, legal hold, DLP)** — Teams (Microsoft
  Purview/eDiscovery Premium) and Meet (Google Vault) have deep, mature
  compliance suites; Zoom and Webex compliance capability leans more on
  admin dashboards, retention config, and (for Webex) a dedicated compliance
  officer role in Control Hub, with less emphasis on document-discovery-style
  eDiscovery. The unevenness across vendors — this is clearly an
  enterprise/regulated-industry add-on, not something every meeting product
  needs — supports treating it as a differentiator for Waddle rather than a
  blocking gap.

## 4. Sources

**Zoom**
- https://support.zoom.us/hc/en-us/articles/201362823-Hosting-large-meetings (redirected to https://support.zoom.com/hc/en/article?id=zm_kb&sysparm_article=KB0065116)
- https://support.zoom.us/hc/en-us/articles/200917029-Getting-started-with-Zoom-Webinars (redirected to https://support.zoom.com/hc/en/article?id=zm_kb&sysparm_article=KB0064444)
- https://support.zoom.us/hc/en-us/articles/201800359-Zoom-Cloud-Service-Status
- https://uptime.zoom.us/
- https://explore.zoom.us/premier-support-terms
- Zoom AI Companion / transcription and E2EE support articles located via support.zoom.us search (live captions ~46 languages, cloud-recording transcripts 12 languages, E2EE feature/limitation pages)
- Zoom noise-suppression support section: https://support.zoom.us/hc/en-us/sections/4413777090061-Audio-Features

**Microsoft Teams / Microsoft 365**
- https://learn.microsoft.com/en-us/microsoftteams/meeting-recording
- https://learn.microsoft.com/en-us/microsoftteams/tmr-meeting-recording-change
- https://learn.microsoft.com/en-us/microsoftteams/manage-teams-recording-compliance
- https://learn.microsoft.com/en-us/microsoftteams/manage-teams-recording-expiration-policy
- https://learn.microsoft.com/en-us/microsoftteams/view-only-meeting-experience
- https://learn.microsoft.com/en-us/microsoftteams/overview-meetings-webinars-town-halls
- https://learn.microsoft.com/en-us/microsoftteams/meeting-webinar-town-hall-feature-comparison
- https://learn.microsoft.com/en-us/microsoftteams/limits-specifications-teams
- https://learn.microsoft.com/en-us/microsoftteams/meeting-transcription-captions
- https://learn.microsoft.com/en-us/microsoftteams/end-to-end-encrypted-meetings
- https://learn.microsoft.com/en-us/microsoftteams/teams-end-to-end-encryption
- https://learn.microsoft.com/en-us/microsoft-365/compliance/ediscovery-teams-legal-hold
- https://learn.microsoft.com/en-us/purview/ediscovery-manage-legal-investigations
- https://learn.microsoft.com/en-us/microsoftteams/security-compliance-overview

**Google Meet / Google Workspace**
- https://support.google.com/meet/answer/7317473
- https://support.google.com/meet/answer/13396001
- https://support.google.com/meet/answer/13054147 (breakout rooms)
- https://support.google.com/meet/answer/10885841 (co-hosts)
- https://support.google.com/a/answer/16398156 (waiting room admin)
- https://support.google.com/vault/answer/7682297 (retain Meet data with Vault)
- https://support.google.com/vault/answer/7657464 (holds on Drive/Meet/Sites)
- https://support.google.com/vault/answer/6127699 (supported services & data types)
- https://support.google.com/a/answer/13851268 (Vault log events)
- https://support.google.com/meet/answer/15077804 (live captions)
- https://support.google.com/meet/answer/12387251 (call/meeting encryption)
- https://support.google.com/meet/answer/11605714 (client-side encryption)
- https://workspace.google.com/terms/sla/

**Slack (Huddles)**
- https://slack.com/help/articles/4402059015315-Use-huddles-in-Slack
- https://slack.com/features/huddles

**Webex**
- https://help.webex.com/en-us/article/nsj2xpfb/Schedule-a-Webex-Meeting-with-end-to-end-encryption
- https://help.webex.com/en-US/article/5h5d8ab/End-to-end-encryption-with-identity-verification-for-Webex-meetings
- https://help.webex.com/en-us/article/b38ajk/Webex-Suite-Meetings:-KMS-encrypted-meeting-content-and-data-residency
- https://help.webex.com/article/nhxkyce (Webex Suite Meetings Security technical paper)
- https://help.webex.com/en-us/article/nedfu0h/Deploy-Zero-Trust-Meetings
- https://help.webex.com/en-us/article/61u6p3/Federal-Information-Processing-Standards-FIPS-for-your-Cisco-Webex-Site
- https://help.webex.com/en-us/article/WBX26731/What-is-the-Maximum-Number-of-Participants-in-a-Webex-Session-or-Call
- https://help.webex.com/en-us/article/h00r1p/view-the-maximum-participant-limits-for-your-webex-site
- https://help.webex.com/en-us/article/qbp6ek/Compare-experiences-in-Webex-Webinars
- https://help.webex.com/en-us/article/nue0dlp/Enable-the-Auto-Deletion-Policy-for-Webex-Recordings
- https://help.webex.com/en-us/article/nlbihhs/Manage-retention-policies-in-Control-Hub
- https://help.webex.com/default/article/nvxjt52/Manage-compliance-data-for-legal-hold
- https://help.webex.com/en-us/article/nqzpeei/show-real-time-translation-and-transcription-in-meetings-and-webinars
- https://help.webex.com/en-us/article/4gbs15/Compare-Webex-Assistant-and-automated-closed-captions
- https://help.webex.com/en-us/article/n70a8os/Remove-background-noise-during-Webex-meetings-or-webinars
- https://help.webex.com/article/3jqfrs/Webex-App-|-Webex-Smart-Audio-in-calls,-meetings,-and-webinars
