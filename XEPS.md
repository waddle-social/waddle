# XEP Evaluation for Waddle

This document lists XEPs from the ./xeps directory relevant to Waddle, organized by category. Focus is on **core protocol XEPs** and XEPs important for a **Discord/Slack/Community application**.

Legend: Status shown in parentheses. **Bold** = especially important for Waddle. **Chat ✓** = integrated in `chat/` frontend.

---

## Core Protocol

These are foundational XEPs required for a functioning XMPP implementation.

| XEP | Title | Status |
|-----|-------|--------|
| **0004** | **Data Forms** | Final | Implemented | Chat ✓ |
| 0009 | Jabber-RPC | Final |
| **0012** | **Last Activity** | Final | Implemented | Chat N/A |
| **0030** | **Service Discovery** | Final | Implemented | Chat ✓ |
| 0033 | Extended Stanza Addressing | Draft |
| **0047** | **In-Band Bytestreams** | Final | Implemented | Chat N/A |
| **0050** | **Ad-Hoc Commands** | Draft | Implemented | Chat N/A |
| 0053 | XMPP Registrar Function | Active |
| **0059** | **Result Set Management** | Draft | Implemented | Chat N/A |
| **0060** | **Publish-Subscribe** | Draft |
| 0065 | SOCKS5 Bytestreams | Draft |
| 0066 | Out of Band Data | Draft |
| **0077** | **In-Band Registration** | Final |
| 0079 | Advanced Message Processing | Draft |
| **0082** | **XMPP Date and Time Profiles** | Active |
| **0085** | **Chat State Notifications** | Final | Implemented | Chat ✓ |
| **0106** | **JID Escaping** | Draft | Implemented | Chat N/A |
| **0114** | **Jabber Component Protocol** | Active |
| **0115** | **Entity Capabilities** | Draft |
| 0122 | Data Forms Validation | Draft |
| 0124 | BOSH | Draft |
| **0128** | **Service Discovery Extensions** | Active |
| 0131 | Stanza Headers and Internet Metadata | Draft |
| 0141 | Data Forms Layout | Draft |
| 0144 | Roster Item Exchange | Draft |
| **0156** | **Discovering Alternative XMPP Connection Methods** | Draft |
| **0163** | **Personal Eventing Protocol** | Draft |
| **0174** | **Serverless Messaging** | Final |
| 0182 | Application-Specific Error Conditions | Active |
| **0198** | **Stream Management** | Draft |
| **0199** | **XMPP Ping** | Final |
| **0202** | **Entity Time** | Final | Implemented | Chat N/A |
| **0203** | **Delayed Delivery** | Final | Implemented | Chat N/A |
| 0206 | XMPP Over BOSH | Draft |
| **0220** | **Server Dialback** | Draft |
| 0227 | Portable Import/Export Format | Draft |
| 0231 | Bits of Binary | Draft |
| 0249 | Direct MUC Invitations | Draft |
| 0258 | Security Labels in XMPP | Draft |
| **0288** | **Bidirectional S2S Connections** | Draft |
| **0297** | **Stanza Forwarding** | Draft | Implemented | Chat N/A |
| **0300** | **Cryptographic Hash Functions** | Draft | Implemented | Chat N/A |
| **0334** | **Message Processing Hints** | Draft | Implemented | Chat N/A |
| **0352** | **Client State Indication** | Draft |
| **0359** | **Unique and Stable Stanza IDs** | Experimental |
| **0363** | **HTTP File Upload** | Draft |
| **0368** | **SRV records for XMPP over TLS** | Draft |
| **0386** | **Bind 2** | Draft |
| **0388** | **Extensible SASL Profile** | Draft |
| **0392** | **Consistent Color Generation** | Draft | Implemented | Chat ✓ |
| **0398** | **User Avatar to vCard-Based Avatars Conversion** | Draft |
| **0440** | **SASL Channel-Binding Type Capability** | Draft |
| **0484** | **Fast Authentication Streamlining Tokens (FAST)** | Proposed |
| **0485** | **PubSub Server Information** | Draft |
| **0490** | **Message Displayed Synchronization** | Draft | Implemented | Chat N/A |
| 0509 | Initial Authentication Pipelining | Experimental |

---

## Messaging

XEPs for rich messaging features expected in a modern community app.

| XEP | Title | Status | Why Evaluate |
|-----|-------|--------|--------------|
| **0085** | **Chat State Notifications** | Final | Typing indicators | Implemented | Chat ✓ |
| **0184** | **Message Delivery Receipts** | Draft | Delivery confirmation | Implemented | Chat ✓ |
| **0280** | **Message Carbons** | Draft | Multi-device message sync |
| **0308** | **Last Message Correction** | Draft | Edit messages | Implemented | Chat ✓ |
| **0313** | **Message Archive Management (MAM)** | Draft | Message history / scrollback |
| **0333** | **Displayed Markers** | Draft | Read receipts | Implemented | Chat ✓ |
| **0334** | **Message Processing Hints** | Draft | Control server-side processing | Implemented | Chat N/A |
| **0359** | **Unique and Stable Stanza IDs** | Experimental | Stable message references | Implemented | Chat N/A |
| **0363** | **HTTP File Upload** | Draft | File/image uploads |
| **0372** | **References** | Experimental | @mentions in messages | Implemented | Chat ✓ |
| 0382 | Spoiler messages | Deferred | Content warnings / spoiler tags |
| **0393** | **Message Styling** | Draft | Bold, italic, code formatting | Implemented | Chat ✓ |
| 0394 | Message Markup | Experimental | Structured text markup |
| **0424** | **Message Retraction** | Proposed | Delete messages | Implemented | Chat ✓ |
| **0425** | **Moderated Message Retraction** | Experimental | Mod-delete messages | Implemented | Chat ✓ |
| 0428 | Fallback Indication | Experimental | Graceful degradation |
| **0444** | **Message Reactions** | Experimental | Emoji reactions | Implemented | Chat ✓ |
| **0461** | **Message Replies** | Experimental | Threaded replies |
| **0490** | **Message Displayed Synchronization** | Draft | Cross-device read state | Implemented | Chat N/A |
| **0513** | **Explicit Mentions** | Experimental | @user / @role mentions | Implemented | Chat ✓ |
| 0511 | Link Metadata | Experimental | Link previews / unfurling |
| 0481 | Content Types in Messages | Experimental | Rich content types |

---

## Multi-User Chat / Channels / Spaces

XEPs for group chat, channels, and organizational structure (the core of a community app).

| XEP | Title | Status | Why Evaluate |
|-----|-------|--------|--------------|
| **0045** | **Multi-User Chat (MUC)** | Draft | Foundation for group chat |
| 0249 | Direct MUC Invitations | Draft | Invite links |
| 0306 | Extensible Status Conditions for MUC | Deferred | Custom room status |
| 0307 | Unique Room Names for MUC | Deferred | Prevent room name collisions |
| **0317** | **Hats** | Experimental | Roles/badges (admin, mod, etc.) | Implemented | Chat ✓ |
| **0369** | **MIX (Mediated Information eXchange)** | Experimental | Next-gen group chat (MUC successor) |
| 0403 | MIX: Presence Support | Deferred | MIX presence |
| 0404 | MIX: JID Hidden Channels | Deferred | Anonymous channels |
| 0405 | MIX: Participant Server Requirements | Experimental | MIX server-side |
| 0406 | MIX: Administration | Deferred | MIX admin |
| 0407 | MIX: Miscellaneous Capabilities | Deferred | MIX extras |
| 0408 | MIX: Co-existence with MUC | Deferred | MIX/MUC bridge |
| **0410** | **MUC Self-Ping** | Draft | Detect disconnection from rooms | Implemented | Chat ✓ |
| **0421** | **Occupant Identifiers for Semi-Anonymous MUCs** | Draft | Stable anonymous identities | Implemented | Chat N/A |
| **0433** | **Extended Channel Search** | Deferred | Channel/room discovery | Implemented | Chat N/A |
| 0436 | MUC Presence Versioning | Deferred | Efficient presence updates |
| **0437** | **Room Activity Indicators** | Deferred | Unread / activity badges | Implemented | Chat ✓ |
| **0452** | **MUC Mention Notifications** | Experimental | @mention alerts | Implemented | Chat N/A |
| 0463 | MUC Affiliations Versioning | Experimental | Efficient role sync |
| **0486** | **MUC Avatars** | Experimental | Room/channel icons | Implemented |
| **0488** | **MUC Token Invite** | Experimental | Invite links with tokens | Implemented | Chat N/A |
| **0492** | **Chat Notification Settings** | Experimental | Per-channel notification prefs | Implemented | Chat N/A |
| **0500** | **MUC Slow Mode** | Experimental | Rate limiting in channels | Implemented | Chat ✓ |
| **0502** | **MUC Activity Indicator** | Experimental | Active room indicators | Implemented | Chat ✓ |
| **0503** | **Server-side Spaces** | Experimental | Discord-like server/space structure |
| **0508** | **Forums** | Experimental | Forum-style threaded discussions | Implemented | Chat N/A |

---

## User Profile / Presence / Identity

| XEP | Title | Status | Why Evaluate |
|-----|-------|--------|--------------|
| **0012** | **Last Activity** | Final | "Last seen" | Implemented | Chat N/A |
| **0054** | **vcard-temp** | Active | User profiles (legacy) |
| **0084** | **User Avatar** | Draft | Profile pictures |
| **0092** | **Software Version** | Draft | Client identification | Implemented | Chat ✓ |
| 0107 | User Mood | Draft | Status moods |
| 0108 | User Activity | Draft | Custom activity status |
| **0153** | **vCard-Based Avatars** | Active | Avatar via vCard |
| **0172** | **User Nickname** | Draft | Display names | Implemented | Chat N/A |
| **0292** | **vCard4 Over XMPP** | Experimental | Modern user profiles | Implemented | Chat N/A |
| **0319** | **Last User Interaction in Presence** | Draft | Idle detection | Implemented | Chat N/A |
| **0392** | **Consistent Color Generation** | Draft | Deterministic user colors | Implemented | Chat ✓ |

---

## Bookmarks / Organization

| XEP | Title | Status | Why Evaluate |
|-----|-------|--------|--------------|
| **0402** | **PEP Native Bookmarks** | Draft | Channel/room bookmarks |
| **0469** | **Bookmark Pinning** | Experimental | Pin favorite channels | Implemented | Chat N/A |

---

## File Sharing / Media

| XEP | Title | Status | Why Evaluate |
|-----|-------|--------|--------------|
| **0363** | **HTTP File Upload** | Draft | File uploads |
| **0446** | **File Metadata Element** | Experimental | File metadata (size, type, etc.) | Implemented | Chat ✓ |
| **0447** | **Stateless File Sharing** | Experimental | Share files without state | Implemented | Chat ✓ |
| 0448 | Encryption for Stateless File Sharing | Experimental | Encrypted file sharing |
| **0449** | **Stickers** | Experimental | Sticker packs | Implemented | Chat ✓ |
| 0498 | Pubsub File Sharing | Experimental | Shared file repositories |
| 0264 | Jingle Content Thumbnails | Experimental | Image thumbnails |

---

## Push Notifications / Inbox

| XEP | Title | Status | Why Evaluate |
|-----|-------|--------|--------------|
| **0357** | **Push Notifications** | Deferred | Mobile push (APNs/FCM) |
| **0430** | **Inbox** | Deferred | Unified inbox / unread counts |

---

## Encryption / Security

| XEP | Title | Status | Why Evaluate |
|-----|-------|--------|--------------|
| **0384** | **OMEMO Encryption** | Experimental | E2E encryption (Signal protocol) |
| 0373 | OpenPGP for XMPP | Experimental | PGP-based encryption |
| **0420** | **Stanza Content Encryption** | Experimental | Generic E2E encryption envelope |
| 0380 | Explicit Message Encryption | Deferred | Signal encrypted message type |
| 0434 | Trust Messages (TM) | Experimental | Key trust management |
| 0450 | Automatic Trust Management (ATM) | Experimental | TOFU-style trust |
| 0474 | SASL SCRAM Downgrade Protection | Experimental | Auth security |
| 0510 | E2E Encrypted Contacts Metadata | Experimental | Private contact lists |

---

## Moderation / Administration / Anti-Abuse

| XEP | Title | Status | Why Evaluate |
|-----|-------|--------|--------------|
| **0050** | **Ad-Hoc Commands** | Draft | Admin commands | Implemented | Chat N/A |
| **0133** | **Service Administration** | Active | Server admin |
| **0191** | **Blocking Command** | Draft | Block users |
| **0377** | **Blocking Command Reports** | Proposed | Report abuse | Implemented | Chat N/A |
| **0425** | **Moderated Message Retraction** | Experimental | Mod-delete messages | Implemented | Chat ✓ |
| **0489** | **Reporting Account Affiliations** | Experimental | Trust signals |
| **0500** | **MUC Slow Mode** | Experimental | Rate limiting |
| 0158 | CAPTCHA Forms | Draft | Bot prevention |
| 0504 | Data Policy | Experimental | Data handling policies |

---

## Invitations / Onboarding

| XEP | Title | Status | Why Evaluate |
|-----|-------|--------|--------------|
| **0379** | **Pre-Authenticated Roster Subscription** | Proposed | Easy contact adding |
| **0401** | **Ad-hoc Account Invitation Generation** | Experimental | Generate invite links | Implemented | Chat N/A |
| **0445** | **Pre-Authenticated In-Band Registration** | Proposed | Register via invite | Implemented | Chat N/A |
| **0488** | **MUC Token Invite** | Experimental | Room invite tokens |
| 0493 | OAuth Client Login | Experimental | OAuth-based auth |
| 0494 | Client Access Management | Experimental | Session management |

---

## Social / Community Features

| XEP | Title | Status | Why Evaluate |
|-----|-------|--------|--------------|
| 0277 | Microblogging over XMPP | Deferred | Social feed posts |
| **0470** | **Pubsub Attachments** | Experimental | Reactions/comments on posts | Implemented | Chat N/A |
| **0471** | **Calendar Events** | Experimental | Community events | Implemented | Chat N/A |
| **0472** | **Pubsub Social Feed** | Experimental | Activity feeds | Implemented | Chat N/A |
| **0501** | **Pubsub Stories** | Experimental | Ephemeral stories | Implemented | Chat N/A |

---

## Search / Discovery

| XEP | Title | Status | Why Evaluate |
|-----|-------|--------|--------------|
| 0055 | Jabber Search | Active | User/entity search |
| **0215** | **External Service Discovery** | Draft | External services | Implemented | Chat N/A |
| **0433** | **Extended Channel Search** | Deferred | Room/channel search |
| **0431** | **Full Text Search in MAM** | Deferred | Message search | Implemented |
| 0499 | Pubsub Extended Discovery | Experimental | Pubsub node search |

---

## All XEPs (Complete List)

<details>
<summary>Click to expand full list of all 513 XEPs</summary>

| XEP | Title | Status | Type |
|-----|-------|--------|------|
| 0001 | XMPP Extension Protocols | Active | Procedural |
| 0002 | Special Interest Groups (SIGs) | Active | Procedural |
| 0003 | Proxy Accept Socket Service (PASS) | Obsolete | Historical |
| 0004 | Data Forms | Final | Standards Track |
| 0005 | Jabber Interest Groups | Obsolete | Informational |
| 0006 | Profiles | Obsolete | SIG Formation |
| 0007 | Conferencing SIG | Obsolete | SIG Proposal |
| 0008 | IQ-Based Avatars | Obsolete | Historical |
| 0009 | Jabber-RPC | Final | Standards Track |
| 0010 | Whiteboarding SIG | Obsolete | SIG Formation |
| 0011 | Jabber Browsing | Obsolete | Historical |
| 0012 | Last Activity | Final | Standards Track |
| 0013 | Flexible Offline Message Retrieval | Deprecated | Standards Track |
| 0014 | Message Tone | Rejected | Standards Track |
| 0015 | Account Transfer | Rejected | Standards Track |
| 0016 | Privacy Lists | Deprecated | Standards Track |
| 0017 | Naive Packet Framing Protocol | Rejected | Informational |
| 0018 | Invisible Presence | Rejected | Informational |
| 0019 | Streamlining the SIGs | Active | Procedural |
| 0020 | Feature Negotiation | Deprecated | Standards Track |
| 0021 | Jabber Event Notification Service (ENS) | Retracted | Standards Track |
| 0022 | Message Events | Obsolete | Historical |
| 0023 | Message Expiration | Obsolete | Historical |
| 0024 | Publish/Subscribe | Retracted | Standards Track |
| 0025 | Jabber HTTP Polling | Obsolete | Historical |
| 0026 | Internationalization (I18N) | Retracted | Standards Track |
| 0027 | Current Jabber OpenPGP Usage | Obsolete | Historical |
| 0028 | No Such XEP | Retracted | Informational |
| 0029 | Definition of Jabber Identifiers (JIDs) | Retracted | Standards Track |
| 0030 | Service Discovery | Final | Standards Track |
| 0031 | A Framework For Securing Jabber Conversations | Deferred | Standards Track |
| 0032 | Jabber URI Scheme | Retracted | Standards Track |
| 0033 | Extended Stanza Addressing | Draft | Standards Track |
| 0034 | SASL Integration | Retracted | Standards Track |
| 0035 | SSL/TLS Integration | Retracted | Standards Track |
| 0036 | Pub-Sub Subscriptions | Retracted | Standards Track |
| 0037 | DSPS - Data Stream Proxy Service | Rejected | Standards Track |
| 0038 | Icon Styles | Obsolete | Standards Track |
| 0039 | Statistics Gathering | Deferred | Standards Track |
| 0040 | Jabber Robust Publish-Subscribe | Retracted | Standards Track |
| 0041 | Reliable Entity Link | Retracted | Standards Track |
| 0042 | Jabber OOB Broadcast Service (JOBS) | Retracted | Standards Track |
| 0043 | Jabber Database Access | Retracted | Standards Track |
| 0044 | Full Namespace Support for XML Streams | Deferred | Standards Track |
| 0045 | Multi-User Chat | Draft | Standards Track |
| 0046 | DTCP | Retracted | Standards Track |
| 0047 | In-Band Bytestreams | Final | Standards Track |
| 0048 | Bookmarks | Deprecated | Standards Track |
| 0049 | Private XML Storage | Active | Historical |
| 0050 | Ad-Hoc Commands | Draft | Standards Track |
| 0051 | Connection Transfer | Obsolete | Standards Track |
| 0052 | File Transfer | Retracted | Standards Track |
| 0053 | XMPP Registrar Function | Active | Procedural |
| 0054 | vcard-temp | Active | Historical |
| 0055 | Jabber Search | Active | Historical |
| 0056 | Business Data Interchange | Deferred | Standards Track |
| 0057 | Extended Roster | Retracted | Standards Track |
| 0058 | Multi-User Text Editing | Deferred | Standards Track |
| 0059 | Result Set Management | Draft | Standards Track |
| 0060 | Publish-Subscribe | Draft | Standards Track |
| 0061 | Shared Notes | Deferred | Informational |
| 0062 | Packet Filtering | Deferred | Informational |
| 0063 | Basic Filtering Operations | Deferred | Informational |
| 0064 | XPath Filtering | Deferred | Informational |
| 0065 | SOCKS5 Bytestreams | Draft | Standards Track |
| 0066 | Out of Band Data | Draft | Standards Track |
| 0067 | Stock Data Transmission | Deferred | Standards Track |
| 0068 | Field Standardization for Data Forms | Active | Informational |
| 0069 | Compliance SIG | Deferred | SIG Formation |
| 0070 | Verifying HTTP Requests via XMPP | Draft | Standards Track |
| 0071 | XHTML-IM | Deprecated | Standards Track |
| 0072 | SOAP Over XMPP | Draft | Standards Track |
| 0073 | Basic IM Protocol Suite | Obsolete | Standards Track |
| 0074 | Simple Access Control | Retracted | Standards Track |
| 0075 | Jabber Object Access Protocol (JOAP) | Deferred | Standards Track |
| 0076 | Malicious Stanzas | Active | Humorous |
| 0077 | In-Band Registration | Final | Standards Track |
| 0078 | Non-SASL Authentication | Obsolete | Standards Track |
| 0079 | Advanced Message Processing | Draft | Standards Track |
| 0080 | User Location | Draft | Standards Track |
| 0081 | Jabber MIME Type | Retracted | Standards Track |
| 0082 | XMPP Date and Time Profiles | Active | Informational |
| 0083 | Nested Roster Groups | Active | Informational |
| 0084 | User Avatar | Draft | Standards Track |
| 0085 | Chat State Notifications | Final | Standards Track |
| 0086 | Error Condition Mappings | Deprecated | Informational |
| 0087 | Stream Initiation | Retracted | Standards Track |
| 0088 | Client Webtabs | Deferred | Informational |
| 0089 | Generic Alerts | Deferred | Standards Track |
| 0090 | Legacy Entity Time | Obsolete | Historical |
| 0091 | Legacy Delayed Delivery | Obsolete | Historical |
| 0092 | Software Version | Draft | Standards Track |
| 0093 | Roster Item Exchange | Deprecated | Historical |
| 0094 | Agent Information | Obsolete | Historical |
| 0095 | Stream Initiation | Deprecated | Standards Track |
| 0096 | SI File Transfer | Deprecated | Standards Track |
| 0097 | iCal Envelope | Deferred | Standards Track |
| 0098 | Enhanced Private XML Storage | Deferred | Standards Track |
| 0099 | IQ Query Action Protocol | Deferred | Standards Track |
| 0100 | Gateway Interaction | Active | Informational |
| 0101 | HTTP Authentication using Jabber Tickets | Deferred | Standards Track |
| 0102 | Security Extensions | Deferred | Standards Track |
| 0103 | URL Address Information | Deferred | Standards Track |
| 0104 | HTTP Scheme for URL Data | Deferred | Standards Track |
| 0105 | Tree Transfer Stream Initiation Profile | Deferred | Standards Track |
| 0106 | JID Escaping | Draft | Standards Track |
| 0107 | User Mood | Draft | Standards Track |
| 0108 | User Activity | Draft | Standards Track |
| 0109 | Out-of-Office Messages | Deferred | Standards Track |
| 0110 | Generic Maps | Deferred | Standards Track |
| 0111 | A Transport for Initiating and Negotiating Sessions (TINS) | Retracted | Standards Track |
| 0112 | User Physical Location | Obsolete | Standards Track |
| 0113 | Simple Whiteboarding | Deferred | Informational |
| 0114 | Jabber Component Protocol | Active | Historical |
| 0115 | Entity Capabilities | Draft | Standards Track |
| 0116 | Encrypted Session Negotiation | Deferred | Standards Track |
| 0117 | Intermediate IM Protocol Suite | Obsolete | Standards Track |
| 0118 | User Tune | Draft | Standards Track |
| 0119 | Extended Presence Protocol Suite | Retracted | Standards Track |
| 0120 | Infobits | Retracted | Standards Track |
| 0121 | Dublin Core Infobits Mapping | Retracted | Informational |
| 0122 | Data Forms Validation | Draft | Standards Track |
| 0123 | Entity Metadata | Retracted | Standards Track |
| 0124 | Bidirectional-streams Over Synchronous HTTP (BOSH) | Draft | Standards Track |
| 0125 | vCard Infobits Mapping | Retracted | Informational |
| 0126 | Invisibility | Deprecated | Informational |
| 0127 | Common Alerting Protocol (CAP) Over XMPP | Active | Informational |
| 0128 | Service Discovery Extensions | Active | Informational |
| 0129 | WebDAV File Transfers | Deferred | Standards Track |
| 0130 | Waiting Lists | Deprecated | Historical |
| 0131 | Stanza Headers and Internet Metadata | Draft | Standards Track |
| 0132 | Presence Obtained via Kinesthetic Excitation (POKE) | Active | Humorous |
| 0133 | Service Administration | Active | Informational |
| 0134 | XMPP Design Guidelines | Active | Informational |
| 0135 | File Sharing | Deferred | Standards Track |
| 0136 | Message Archiving | Deprecated | Standards Track |
| 0137 | Publishing Stream Initiation Requests | Deprecated | Standards Track |
| 0138 | Stream Compression | Obsolete | Standards Track |
| 0139 | Security SIG | Retracted | SIG Formation |
| 0140 | Shared Groups | Retracted | Informational |
| 0141 | Data Forms Layout | Draft | Standards Track |
| 0142 | Workgroup Queues | Deferred | Standards Track |
| 0143 | Guidelines for Authors of XMPP Extension Protocols | Active | Procedural |
| 0144 | Roster Item Exchange | Draft | Standards Track |
| 0145 | Annotations | Active | Historical |
| 0146 | Remote Controlling Clients | Obsolete | Informational |
| 0147 | XMPP URI Scheme Query Components | Active | Informational |
| 0148 | Instant Messaging Intelligence Quotient (IM IQ) | Active | Humorous |
| 0149 | Time Periods | Active | Informational |
| 0150 | Use of Entity Tags in XMPP Extensions | Deferred | Informational |
| 0151 | Virtual Presence | Deferred | Standards Track |
| 0152 | Reachability Addresses | Draft | Standards Track |
| 0153 | vCard-Based Avatars | Active | Historical |
| 0154 | User Profile | Deferred | Standards Track |
| 0155 | Stanza Session Negotiation | Draft | Standards Track |
| 0156 | Discovering Alternative XMPP Connection Methods | Draft | Standards Track |
| 0157 | Contact Addresses for XMPP Services | Active | Informational |
| 0158 | CAPTCHA Forms | Draft | Standards Track |
| 0159 | Spim-Blocking Control | Deferred | Standards Track |
| 0160 | Best Practices for Handling Offline Messages | Active | Informational |
| 0161 | Abuse Reporting | Deferred | Standards Track |
| 0162 | Best Practices for Roster and Subscription Management | Deferred | Informational |
| 0163 | Personal Eventing Protocol | Draft | Standards Track |
| 0164 | vCard Filtering | Deferred | Standards Track |
| 0165 | Best Practices to Discourage JID Mimicking | Deferred | Informational |
| 0166 | Jingle | Draft | Standards Track |
| 0167 | Jingle RTP Sessions | Draft | Standards Track |
| 0168 | Resource Application Priority | Deferred | Standards Track |
| 0169 | Twas The Night Before Christmas (Jabber Version) | Active | Humorous |
| 0170 | Recommended Order of Stream Feature Negotiation | Active | Informational |
| 0171 | Language Translation | Draft | Standards Track |
| 0172 | User Nickname | Draft | Standards Track |
| 0173 | Pubsub Subscription Storage | Deferred | Historical |
| 0174 | Serverless Messaging | Final | Standards Track |
| 0175 | Best Practices for Use of SASL ANONYMOUS | Active | Informational |
| 0176 | Jingle ICE-UDP Transport Method | Draft | Standards Track |
| 0177 | Jingle Raw UDP Transport Method | Draft | Standards Track |
| 0178 | Best Practices for Use of SASL EXTERNAL with Certificates | Active | Informational |
| 0179 | Jingle IAX Transport Method | Deferred | Standards Track |
| 0180 | Jingle Video via RTP | Retracted | Standards Track |
| 0181 | Jingle DTMF | Deferred | Standards Track |
| 0182 | Application-Specific Error Conditions | Active | Procedural |
| 0183 | Jingle Telepathy Transport | Active | Humorous |
| 0184 | Message Delivery Receipts | Draft | Standards Track |
| 0185 | Dialback Key Generation and Validation | Active | Informational |
| 0186 | Invisible Command | Deferred | Standards Track |
| 0187 | Offline Encrypted Sessions | Deferred | Standards Track |
| 0188 | Cryptographic Design of Encrypted Sessions | Deferred | Informational |
| 0189 | Public Key Publishing | Deferred | Standards Track |
| 0190 | Best Practice for Closing Idle Streams | Obsolete | Informational |
| 0191 | Blocking Command | Draft | Standards Track |
| 0192 | Proposed Stream Feature Improvements | Obsolete | Standards Track |
| 0193 | Proposed Resource Binding Improvements | Obsolete | Standards Track |
| 0194 | User Chatting | Deferred | Standards Track |
| 0195 | User Browsing | Deferred | Standards Track |
| 0196 | User Gaming | Deferred | Standards Track |
| 0197 | User Viewing | Deferred | Standards Track |
| 0198 | Stream Management | Draft | Standards Track |
| 0199 | XMPP Ping | Final | Standards Track |
| 0200 | Stanza Encryption | Deferred | Standards Track |
| 0201 | Best Practices for Message Threads | Active | Informational |
| 0202 | Entity Time | Final | Standards Track |
| 0203 | Delayed Delivery | Final | Standards Track |
| 0204 | Collaborative Data Objects | Deferred | Standards Track |
| 0205 | Best Practices to Discourage Denial of Service Attacks | Active | Informational |
| 0206 | XMPP Over BOSH | Draft | Standards Track |
| 0207 | XMPP Eventing via Pubsub | Active | Humorous |
| 0208 | Bootstrapping Implementation of Jingle | Retracted | Informational |
| 0209 | Metacontacts | Deferred | Standards Track |
| 0210 | Requirements for Encrypted Sessions | Deferred | Standards Track |
| 0211 | XMPP Basic Client 2008 | Obsolete | Standards Track |
| 0212 | XMPP Basic Server 2008 | Obsolete | Standards Track |
| 0213 | XMPP Intermediate IM Client 2008 | Obsolete | Standards Track |
| 0214 | File Repository and Sharing | Deferred | Standards Track |
| 0215 | External Service Discovery | Draft | Standards Track |
| 0216 | XMPP Intermediate IM Server 2008 | Obsolete | Standards Track |
| 0217 | Simplified Encrypted Session Negotiation | Deferred | Standards Track |
| 0218 | Bootstrapping Implementation of Encrypted Sessions | Deferred | Informational |
| 0219 | Hop Check | Retracted | Standards Track |
| 0220 | Server Dialback | Draft | Standards Track |
| 0221 | Data Forms Media Element | Draft | Standards Track |
| 0222 | Persistent Storage of Public Data via PubSub | Active | Informational |
| 0223 | Persistent Storage of Private Data via PubSub | Active | Informational |
| 0224 | Attention | Draft | Standards Track |
| 0225 | Component Connections | Deferred | Standards Track |
| 0226 | Message Stanza Profiles | Deferred | Informational |
| 0227 | Portable Import/Export Format for XMPP-IM Servers | Draft | Standards Track |
| 0228 | Requirements for Shared Editing | Deferred | Standards Track |
| 0229 | Stream Compression with LZW | Obsolete | Standards Track |
| 0230 | Service Discovery Notifications | Deferred | Standards Track |
| 0231 | Bits of Binary | Draft | Standards Track |
| 0232 | Software Information | Deferred | Standards Track |
| 0233 | XMPP Server Registration for use with Kerberos V5 | Draft | Standards Track |
| 0234 | Jingle File Transfer | Deferred | Standards Track |
| 0235 | OAuth Over XMPP | Deferred | Standards Track |
| 0236 | Abuse Reporting | Retracted | Standards Track |
| 0237 | Roster Versioning | Obsolete | Standards Track |
| 0238 | XMPP Protocol Flows for Inter-Domain Federation | Deferred | Informational |
| 0239 | Binary XMPP | Active | Humorous |
| 0240 | Auto-Discovery of JabberIDs | Deferred | Standards Track |
| 0241 | Encryption of Archived Messages | Deferred | Standards Track |
| 0242 | XMPP Client Compliance 2009 | Obsolete | Standards Track |
| 0243 | XMPP Server Compliance 2009 | Obsolete | Standards Track |
| 0244 | IO Data | Deferred | Standards Track |
| 0245 | The /me Command | Active | Informational |
| 0246 | End-to-End XML Streams | Deferred | Standards Track |
| 0247 | Jingle XML Streams | Deferred | Standards Track |
| 0248 | PubSub Collection Nodes | Deferred | Standards Track |
| 0249 | Direct MUC Invitations | Draft | Standards Track |
| 0250 | C2C Authentication Using TLS | Deferred | Standards Track |
| 0251 | Jingle Session Transfer | Deferred | Standards Track |
| 0252 | BOSH Script Syntax | Deferred | Historical |
| 0253 | PubSub Chaining | Deferred | Standards Track |
| 0254 | PubSub Queueing | Deferred | Standards Track |
| 0255 | Location Query | Deferred | Standards Track |
| 0256 | Last Activity in Presence | Deprecated | Standards Track |
| 0257 | Client Certificate Management for SASL EXTERNAL | Deferred | Standards Track |
| 0258 | Security Labels in XMPP | Draft | Standards Track |
| 0259 | Message Mine-ing | Deferred | Standards Track |
| 0260 | Jingle SOCKS5 Bytestreams Transport Method | Draft | Standards Track |
| 0261 | Jingle In-Band Bytestreams Transport Method | Draft | Standards Track |
| 0262 | Use of ZRTP in Jingle RTP Sessions | Draft | Standards Track |
| 0263 | ECO-XMPP | Active | Humorous |
| 0264 | Jingle Content Thumbnails | Experimental | Standards Track |
| 0265 | Out-of-Band Stream Data | Deferred | Standards Track |
| 0266 | Codecs for Jingle Audio | Draft | Standards Track |
| 0267 | Server Buddies | Deferred | Standards Track |
| 0268 | Incident Handling | Deferred | Standards Track |
| 0269 | Jingle Early Media | Deferred | Standards Track |
| 0270 | XMPP Compliance Suites 2010 | Obsolete | Standards Track |
| 0271 | XMPP Nodes | Deferred | Informational |
| 0272 | Multiparty Jingle (Muji) | Experimental | Standards Track |
| 0273 | Stanza Interception and Filtering Technology (SIFT) | Deferred | Standards Track |
| 0274 | Design Considerations for Digital Signatures in XMPP | Deferred | Informational |
| 0275 | Entity Reputation | Deferred | Standards Track |
| 0276 | Presence Decloaking | Deferred | Standards Track |
| 0277 | Microblogging over XMPP | Deferred | Standards Track |
| 0278 | Jingle Relay Nodes | Deferred | Standards Track |
| 0279 | Server IP Check | Deferred | Standards Track |
| 0280 | Message Carbons | Draft | Standards Track |
| 0281 | DMUC1: Distributed Multi-User Chat | Retracted | Standards Track |
| 0282 | DMUC2: Distributed MUC | Deferred | Standards Track |
| 0283 | Moved | Experimental | Standards Track |
| 0284 | Shared XML Editing | Experimental | Standards Track |
| 0285 | Encapsulating Digital Signatures in XMPP | Deferred | Standards Track |
| 0286 | Mobile Considerations on LTE Networks | Active | Informational |
| 0287 | Spim Markers and Reports | Deferred | Standards Track |
| 0288 | Bidirectional Server-to-Server Connections | Draft | Standards Track |
| 0289 | Federated MUC for Constrained Environments | Deferred | Standards Track |
| 0290 | Encapsulated Digital Signatures in XMPP | Deferred | Standards Track |
| 0291 | Service Delegation | Deferred | Standards Track |
| 0292 | vCard4 Over XMPP | Experimental | Standards Track |
| 0293 | Jingle RTP Feedback Negotiation | Draft | Standards Track |
| 0294 | Jingle RTP Header Extensions Negotiation | Draft | Standards Track |
| 0295 | JSON Encodings for XMPP | Active | Humorous |
| 0296 | Best Practices for Resource Locking | Deferred | Informational |
| 0297 | Stanza Forwarding | Draft | Standards Track |
| 0298 | Delivering Conference Information to Jingle Participants (Coin) | Deferred | Standards Track |
| 0299 | Codecs for Jingle Video | Deferred | Standards Track |
| 0300 | Use of Cryptographic Hash Functions in XMPP | Draft | Standards Track |
| 0301 | In-Band Real Time Text | Draft | Standards Track |
| 0302 | XMPP Compliance Suites 2012 | Obsolete | Standards Track |
| 0303 | Commenting | Deferred | Standards Track |
| 0304 | Whitespace Keepalive Negotiation | Deferred | Standards Track |
| 0305 | XMPP Quickstart | Deferred | Standards Track |
| 0306 | Extensible Status Conditions for Multi-User Chat | Deferred | Standards Track |
| 0307 | Unique Room Names for Multi-User Chat | Deferred | Standards Track |
| 0308 | Last Message Correction | Draft | Standards Track |
| 0309 | Service Directories | Deferred | Standards Track |
| 0310 | Presence State Annotations | Deferred | Standards Track |
| 0311 | MUC Fast Reconnect | Deferred | Standards Track |
| 0312 | PubSub Since | Deferred | Standards Track |
| 0313 | Message Archive Management | Draft | Standards Track |
| 0314 | Security Labels in PubSub | Deferred | Standards Track |
| 0315 | Data Forms XML Element | Deferred | Standards Track |
| 0316 | MUC Eventing Protocol | Deferred | Standards Track |
| 0317 | Hats | Experimental | Standards Track |
| 0318 | Best Practices for Client Initiated Presence Probes | Deferred | Informational |
| 0319 | Last User Interaction in Presence | Draft | Standards Track |
| 0320 | Use of DTLS-SRTP in Jingle Sessions | Draft | Standards Track |
| 0321 | Remote Roster Management | Deferred | Standards Track |
| 0322 | Efficient XML Interchange (EXI) Format | Deferred | Standards Track |
| 0323 | Internet of Things - Sensor Data | Retracted | Standards Track |
| 0324 | Internet of Things - Provisioning | Retracted | Standards Track |
| 0325 | Internet of Things - Control | Retracted | Standards Track |
| 0326 | Internet of Things - Concentrators | Retracted | Standards Track |
| 0327 | Rayo | Deferred | Standards Track |
| 0328 | JID Preparation and Validation Service | Deferred | Standards Track |
| 0329 | File Information Sharing | Deferred | Standards Track |
| 0330 | Pubsub Subscription | Deferred | Standards Track |
| 0331 | Data Forms - Color Field Types | Deferred | Standards Track |
| 0332 | HTTP over XMPP transport | Deferred | Standards Track |
| 0333 | Displayed Markers | Draft | Standards Track |
| 0334 | Message Processing Hints | Draft | Standards Track |
| 0335 | JSON Containers | Deferred | Standards Track |
| 0336 | Data Forms - Dynamic Forms | Deferred | Standards Track |
| 0337 | Event Logging over XMPP | Deferred | Standards Track |
| 0338 | Jingle Grouping Framework | Draft | Standards Track |
| 0339 | Source-Specific Media Attributes in Jingle | Draft | Standards Track |
| 0340 | COnferences with LIghtweight BRIdging (COLIBRI) | Deferred | Standards Track |
| 0341 | Rayo CPA | Deferred | Standards Track |
| 0342 | Rayo Fax | Deferred | Standards Track |
| 0343 | Signaling WebRTC datachannels in Jingle | Deferred | Standards Track |
| 0344 | Impact of TLS and DNSSEC on Dialback | Deferred | Standards Track |
| 0345 | Form of Membership Applications | Active | Procedural |
| 0346 | Form Discovery and Publishing | Deferred | Standards Track |
| 0347 | Internet of Things - Discovery | Deferred | Standards Track |
| 0348 | Signing Forms | Deferred | Standards Track |
| 0349 | Rayo Clustering | Deferred | Standards Track |
| 0350 | Data Forms Geolocation Element | Deferred | Standards Track |
| 0351 | Recipient Server Side Notifications Filtering | Deferred | Standards Track |
| 0352 | Client State Indication | Draft | Standards Track |
| 0353 | Jingle Message Initiation | Experimental | Standards Track |
| 0354 | Customizable Message Routing | Deferred | Standards Track |
| 0355 | Namespace Delegation | Experimental | Standards Track |
| 0356 | Privileged Entity | Experimental | Standards Track |
| 0357 | Push Notifications | Deferred | Standards Track |
| 0358 | Publishing Available Jingle Sessions | Deferred | Standards Track |
| 0359 | Unique and Stable Stanza IDs | Experimental | Standards Track |
| 0360 | Nonzas (are not Stanzas) | Rejected | Standards Track |
| 0361 | Zero Handshake Server to Server Protocol | Deferred | Informational |
| 0362 | Raft over XMPP | Deferred | Standards Track |
| 0363 | HTTP File Upload | Draft | Standards Track |
| 0364 | Current Off-the-Record Messaging Usage | Deferred | Informational |
| 0365 | Server to Server communication over STANAG 5066 ARQ | Experimental | Standards Track |
| 0366 | Entity Versioning | Deferred | Standards Track |
| 0367 | Message Attaching | Deferred | Standards Track |
| 0368 | SRV records for XMPP over TLS | Draft | Standards Track |
| 0369 | Mediated Information eXchange (MIX) | Experimental | Standards Track |
| 0370 | Jingle HTTP Transport Method | Deferred | Standards Track |
| 0371 | Jingle ICE Transport Method | Deferred | Standards Track |
| 0372 | References | Experimental | Standards Track |
| 0373 | OpenPGP for XMPP | Experimental | Standards Track |
| 0374 | OpenPGP for XMPP Instant Messaging | Deferred | Standards Track |
| 0375 | XMPP Compliance Suites 2016 | Retracted | Standards Track |
| 0376 | Pubsub Account Management | Deferred | Standards Track |
| 0377 | Blocking Command Reports | Proposed | Standards Track |
| 0378 | OTR Discovery | Deferred | Standards Track |
| 0379 | Pre-Authenticated Roster Subscription | Proposed | Standards Track |
| 0380 | Explicit Message Encryption | Deferred | Standards Track |
| 0381 | Internet of Things Special Interest Group (IoT SIG) | Active | Procedural |
| 0382 | Spoiler messages | Deferred | Standards Track |
| 0383 | Burner JIDs | Experimental | Standards Track |
| 0384 | OMEMO Encryption | Experimental | Standards Track |
| 0385 | Stateless Inline Media Sharing (SIMS) | Deferred | Standards Track |
| 0386 | Bind 2 | Draft | Standards Track |
| 0387 | XMPP Compliance Suites 2018 | Obsolete | Standards Track |
| 0388 | Extensible SASL Profile | Draft | Standards Track |
| 0389 | Extensible In-Band Registration | Experimental | Standards Track |
| 0390 | Entity Capabilities 2.0 | Deferred | Standards Track |
| 0391 | Jingle Encrypted Transports | Deferred | Standards Track |
| 0392 | Consistent Color Generation | Draft | Standards Track |
| 0393 | Message Styling | Draft | Standards Track |
| 0394 | Message Markup | Experimental | Standards Track |
| 0395 | Atomically Compare-And-Publish PubSub Items | Deferred | Standards Track |
| 0396 | Jingle Encrypted Transports - OMEMO | Deferred | Standards Track |
| 0397 | Instant Stream Resumption | Deferred | Standards Track |
| 0398 | User Avatar to vCard-Based Avatars Conversion | Draft | Standards Track |
| 0399 | Client Key Support | Deferred | Standards Track |
| 0400 | Multi-Factor Authentication with TOTP | Deferred | Standards Track |
| 0401 | Ad-hoc Account Invitation Generation | Experimental | Standards Track |
| 0402 | PEP Native Bookmarks | Draft | Standards Track |
| 0403 | Mediated Information eXchange (MIX): Presence Support. | Deferred | Standards Track |
| 0404 | Mediated Information eXchange (MIX): JID Hidden Channels. | Deferred | Standards Track |
| 0405 | Mediated Information eXchange (MIX): Participant Server Requirements | Experimental | Standards Track |
| 0406 | Mediated Information eXchange (MIX): MIX Administration | Deferred | Standards Track |
| 0407 | Mediated Information eXchange (MIX): Miscellaneous Capabilities | Deferred | Standards Track |
| 0408 | Mediated Information eXchange (MIX): Co-existence with MUC | Deferred | Standards Track |
| 0409 | IM Routing-NG | Deferred | Standards Track |
| 0410 | MUC Self-Ping (Schrödinger's Chat) | Draft | Standards Track |
| 0411 | Bookmarks Conversion | Deprecated | Standards Track |
| 0412 | XMPP Compliance Suites 2019 | Obsolete | Standards Track |
| 0413 | Order-By | Experimental | Standards Track |
| 0414 | Cryptographic Hash Function Recommendations for XMPP | Deferred | Informational |
| 0415 | XMPP Over RELOAD (XOR) | Deferred | Standards Track |
| 0416 | E2E Authentication in XMPP | Deferred | Standards Track |
| 0417 | E2E Authentication in XMPP: Certificate Issuance and Revocation | Deferred | Standards Track |
| 0418 | DNS Queries over XMPP (DoX) | Deferred | Standards Track |
| 0419 | Improving Baseline Security in XMPP | Active | Humorous |
| 0420 | Stanza Content Encryption | Experimental | Standards Track |
| 0421 | Occupant identifiers for semi-anonymous MUCs | Draft | Standards Track |
| 0422 | Message Fastening | Deferred | Standards Track |
| 0423 | XMPP Compliance Suites 2020 | Obsolete | Standards Track |
| 0424 | Message Retraction | Proposed | Standards Track |
| 0425 | Moderated Message Retraction | Experimental | Standards Track |
| 0426 | Character counting in message bodies | Experimental | Informational |
| 0427 | MAM Fastening Collation | Deferred | Standards Track |
| 0428 | Fallback Indication | Experimental | Standards Track |
| 0429 | Special Interests Group End to End Encryption | Active | Procedural |
| 0430 | Inbox | Deferred | Standards Track |
| 0431 | Full Text Search in MAM | Deferred | Standards Track |
| 0432 | Simple JSON Messaging | Deferred | Standards Track |
| 0433 | Extended Channel Search | Deferred | Standards Track |
| 0434 | Trust Messages (TM) | Experimental | Standards Track |
| 0435 | Reminders | Deferred | Standards Track |
| 0436 | MUC presence versioning | Deferred | Standards Track |
| 0437 | Room Activity Indicators | Deferred | Standards Track |
| 0438 | ? | ? | ? |
| 0439 | (entity reference) | Deferred | Standards Track |
| 0440 | SASL Channel-Binding Type Capability | Draft | Standards Track |
| 0441 | Message Archive Management Preferences | Experimental | Standards Track |
| 0442 | Pubsub Message Archive Management | Experimental | Standards Track |
| 0443 | XMPP Compliance Suites 2021 | Obsolete | Standards Track |
| 0444 | Message Reactions | Experimental | Standards Track |
| 0445 | Pre-Authenticated In-Band Registration | Proposed | Standards Track |
| 0446 | File metadata element | Experimental | Standards Track |
| 0447 | Stateless file sharing | Experimental | Standards Track |
| 0448 | Encryption for stateless file sharing | Experimental | Standards Track |
| 0449 | Stickers | Experimental | Standards Track |
| 0450 | Automatic Trust Management (ATM) | Experimental | Standards Track |
| 0451 | Stanza Multiplexing | Experimental | Standards Track |
| 0452 | MUC Mention Notifications | Experimental | Standards Track |
| 0453 | DOAP usage in XMPP | Experimental | Informational |
| 0454 | OMEMO Media sharing | Experimental | Historical |
| 0455 | Service Outage Status | Experimental | Standards Track |
| 0456 | Content Rating Labels | Experimental | Standards Track |
| 0457 | Message Fancying | Active | Humorous |
| 0458 | Community Code of Conduct | Active | Procedural |
| 0459 | XMPP Compliance Suites 2022 | Obsolete | Standards Track |
| 0460 | Pubsub Caching Hints | Experimental | Standards Track |
| 0461 | Message Replies | Experimental | Standards Track |
| 0462 | PubSub Type Filtering | Experimental | Standards Track |
| 0463 | MUC Affiliations Versioning | Experimental | Standards Track |
| 0464 | Cookies | Active | Humorous |
| 0465 | Pubsub Public Subscriptions | Experimental | Standards Track |
| 0466 | Ephemeral Messages | Experimental | Standards Track |
| 0467 | XMPP over QUIC | Experimental | Standards Track |
| 0468 | WebSocket S2S | Experimental | Standards Track |
| 0469 | Bookmark Pinning | Experimental | Standards Track |
| 0470 | Pubsub Attachments | Experimental | Standards Track |
| 0471 | Calendar Events | Experimental | Standards Track |
| 0472 | Pubsub Social Feed | Experimental | Standards Track |
| 0473 | OpenPGP for XMPP Pubsub | Experimental | Standards Track |
| 0474 | SASL SCRAM Downgrade Protection | Experimental | Standards Track |
| 0475 | Pubsub Signing | Experimental | Standards Track |
| 0476 | Pubsub Signing: OpenPGP Profile | Experimental | Standards Track |
| 0477 | Pubsub Targeted Encryption | Experimental | Standards Track |
| 0478 | Stream Limits Advertisement | Experimental | Standards Track |
| 0479 | XMPP Compliance Suites 2023 | Experimental | Standards Track |
| 0480 | SASL Upgrade Tasks | Experimental | Standards Track |
| 0481 | Content Types in Messages | Experimental | Standards Track |
| 0482 | Call Invites | Experimental | Standards Track |
| 0483 | HTTP Online Meetings | Experimental | Standards Track |
| 0484 | Fast Authentication Streamlining Tokens | Proposed | Standards Track |
| 0485 | PubSub Server Information | Draft | Standards Track |
| 0486 | MUC Avatars | Experimental | Historical |
| 0487 | Host Meta 2 - One Method To Rule Them All | Experimental | Standards Track |
| 0488 | MUC Token Invite | Experimental | Standards Track |
| 0489 | Reporting Account Affiliations | Experimental | Standards Track |
| 0490 | Message Displayed Synchronization | Draft | Standards Track |
| 0491 | WebXDC | Experimental | Standards Track |
| 0492 | Chat notification settings | Experimental | Standards Track |
| 0493 | OAuth Client Login | Experimental | Informational |
| 0494 | Client Access Management | Experimental | Standards Track |
| 0495 | Happy Eyeballs | Experimental | Standards Track |
| 0496 | Pubsub Node Relationships | Experimental | Standards Track |
| 0497 | Pubsub Extended Subscriptions | Experimental | Standards Track |
| 0498 | Pubsub File Sharing | Experimental | Standards Track |
| 0499 | Pubsub Extended Discovery | Experimental | Standards Track |
| 0500 | MUC Slow Mode | Experimental | Standards Track |
| 0501 | Pubsub Stories | Experimental | Standards Track |
| 0502 | MUC Activity Indicator | Experimental | Standards Track |
| 0503 | Server-side spaces | Experimental | Standards Track |
| 0504 | Data Policy | Experimental | Standards Track |
| 0505 | Data Forms File Input Element | Experimental | Standards Track |
| 0506 | No-reply JIDs | Experimental | Standards Track |
| 0507 | Jingle Content Category | Experimental | Standards Track |
| 0508 | Forums | Experimental | Standards Track |
| 0509 | Initial Authentication Pipelining | Experimental | Standards Track |
| 0510 | End-to-End Encrypted Contacts Metadata | Experimental | Standards Track |
| 0511 | Link Metadata | Experimental | Standards Track |
| 0512 | XMPP as Interpretive Dance | Active | Humorous |
| 0513 | Explicit Mentions | Experimental | Standards Track |

</details>
