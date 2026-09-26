# Huddle: the Waddle Web design system

Status: concept, chosen direction. Concept C from the first round ("Huddle": people first, night ink, teal for action, ember for live) developed into the complete system for Waddle as a community platform. Nothing here changes `chat/` yet.

Canvas with the full system (nine artboards, clickable between screens):
https://claude.ai/artifact/NLk2yGmdN3GxP5niioC6bG

Files in this folder:

- `README.md` — this brief: the idea, brand, tokens, screens, the wire objects they need, stack, migration.
- The token, semantic-token, text-style and recipe config lives in the workspace at `chat/panda.config.ts` (Panda CSS 1.12, registered through `@pandacss/postcss` in `chat/astro.config.mjs`). The Ark UI primitives built on those recipes live under `chat/src/components/ui/`.

## 1. The idea

Presence is the product. Discord shows you channels, Slack shows you messages; Waddle shows you people: who is here, what they are doing, and where you can join them. The rail of who is here never leaves the screen. Rooms are sorted by who is in them, never by name. A discussion page tells you the person who asked is in a huddle right now and lets you join it.

Three rules:

1. **Night is home.** Dark ink lifted from the logo is the canonical theme. Daylight exists and is equal in contrast, but the brand is photographed at night. Toasts and tooltips stay night-coloured in daylight because live things are photographed at night.
2. **Teal acts, ember is alive.** Teal is every button, link and focus ring. Ember-orange is reserved for what is happening right now: a huddle, a speaker, a new person, a mention. Live things may glow; nothing else may.
3. **Quiet is an invitation.** An empty room says "It is quiet in here. Be the one who breaks the silence." Never a zero, never a grey hash.

## 2. Brand

| Element | Decision |
| --- | --- |
| Mark | Keep the speech-bubble penguin. It already sits on a night ground and its beak is the ember. If the mark should change too, the huddle mark from the earlier Hearth exploration (three pills pressed together) is in git history at `68adfbe`. |
| Wordmark | `waddle` in Space Grotesk 700 with an ember full stop. |
| App icon | Night ground with the penguin; teal and daylight variants on the brand board. |
| Display type | Space Grotesk 700/600. Headlines, room and community names, big numbers. |
| Body type | Outfit, already shipped. Geometric enough to sit next to Space Grotesk. |
| Mono | JetBrains Mono, already shipped. Kickers, timestamps, counts, code. |
| Shape | Radii 10 / 16 / pill. 1px `ink.600` borders on cards. One overlay shadow. Glow only on live. |
| Presence | Colour dot: green here, amber away, red do not disturb, hollow offline. A teal ring means in a huddle; a glow on the ring means speaking; three animated bars beside the name mean speaking in a list. |
| Motion | 160ms ease-out for state, 400ms for a card entering. Speaking bars animate; nothing else loops. Reduced motion turns off the glow pulse. |
| Voice | Names the person first. "mara is speaking in Pairing on the Talos cluster." "sol joined and asked their first question. Welcome them." |

### Colour

| Token | Night | Daylight | Role |
| --- | --- | --- | --- |
| `bg` | `#0b1219` | `#f2f6f7` | ground |
| `bg.rail` | `#0f1a24` | `#ffffff` | the people rail and header |
| `surface` | `#121f2b` (the logo) | `#ffffff` | cards |
| `surface.2` | `#1a2a3a` | `#f2f6f7` | raised, hover, active nav |
| `border` | `#26384a` | `#d9e2e6` | |
| `fg` | `#e8eef2` (15:1) | `#0f1a24` | text |
| `fg.muted` | `#9fb0bd` (7.6:1) | `#4a5966` (7.2:1) | secondary text |
| `action` | `#6fd3c7` (ink text 11:1) | `#0b7a6f` (white text 5.2:1) | buttons, links, rings |
| `live` | `#ffa057` (ink text 9:1) | `#ff7f21` (ink text 6.9:1) | huddles, speakers, mentions, new people |
| `live.text` | `#ffa057` | `#b04a06` | live labels on the ground |
| `gold` | `#e9b44c` | `#e9b44c` / text `#8a5a0a` | kudos, helper |
| `moss` | `#7fc98f` | `#2f6b3f` | answered, accepted |
| `danger` | `#d1434b` / text `#ff8b90` | `#d1434b` / text `#b8353d` | leave, remove, report |
| `presence.*` | `#3ddc84` `#f2b036` `#ff8b90` `#6b7f92` | `#1f9d55` `#d97706` `#d1434b` `#c9d4dc` | dots |
| `glow.live` / `glow.action` | ember and teal at 55% / 45% alpha | same | the only allowed glows |

### Type scale (text styles in the config)

`hero` 52 / `display` 38 / `headline` 30 / `title` 18 (Space Grotesk) · `lead` 17 / `body` 15 / `control` 14 (Outfit) · `kicker` 11 mono uppercase tracked 0.08em · `time` 12 mono · `stat` 30 Space Grotesk for big numbers.

## 3. Screens (the artboards)

Layout: a 60px header with the community switcher and pill navigation (**Home · Rooms · Discussions · Events · Members · Library**), a 280px people rail on the left that is always present, the surface in the middle, and a context column on the right that shows the live thing that matters on that screen.

| Screen | People rail shows | Main | Context column |
| --- | --- | --- | --- |
| **Home** | In a huddle (with speaking bars), Around, Away and offline | Greeting with a live summary; **Happening now** (the huddle, the busiest room, the next event); **Needs someone like you** (unanswered questions with the asker's presence) | New this week with a Wave action |
| **Rooms** | same | Rooms as tiles sorted by occupancy; quiet rooms invite; latest across rooms | The huddle you are in: tiles with the speaker highlighted, screen share, controls |
| **Discussion** | In this discussion, then Spaces | Title, author with role tag and live status, body, **accepted answer** in moss with kudos, replies | "Talk about this live": the asker is in a huddle, join it; related Library pages |
| **Members** | (none; the page is the rail) | Directory cards with presence, huddle rings, role, status, kudos; filters Here now / Helpers / New / Everyone | Profile: in-a-huddle banner with Join, badges, rich status, local time, stats, Message and Give kudos |
| **Events** | Going to the next event, with presence; hosts this month | Month grid with today in ember | Next event card with RSVP, 9 of 14 attendees here now, Add to calendar, opens in a room |
| **Public page** | (signed out) | Name, description, live numbers including **41 here right now**, happening right now, recently answered, hosts and helpers | Join, how this community works |
| **Phone** | A horizontal strip of who is here, huddle rings first | Greeting, live huddle card, needs someone, new this week | Bottom tabs Home / People / Rooms / Discuss / Events |
| **Tokens and components** | | Night and daylight side by side, each Ark part next to its recipe | |

### New objects the platform needs on the wire

Everything on the boards is backed by an existing store in `chat/src` (MUC, MAM, PubSub events, vCard4, PEP mood/activity/tune, presence, LiveKit calls) except these:

- **Accepted answer** on a discussion: a `urn:waddle:answer:0` fastening (XEP-0422) referencing the accepted message, set by the asker or a host.
- **Kudos**: XEP-0444 reactions with a reserved emoji for messages; per-person totals as a PEP node or a server-side count.
- **Roles** Host and Helper: MUC affiliations (owner, admin) plus hats (XEP-0317) for Helper.
- **Huddle membership as presence**: "in a huddle in #kubernetes, speaking" needs the in-call activity that `presence/in-call-activity.ts` already tracks to be published, not just shown locally.
- **Public numbers** for the join page (here right now, percent answered, median first reply): computed server side, served to `website/` without an XMPP session.

## 4. Stack: Panda CSS + Ark UI

- `@ark-ui/vue` 5.39.x, Vue ≥ 3.5. Primitives across the boards: Tabs (primary navigation), SegmentGroup (filters), Menu, Popover, Tooltip, Dialog, Avatar, Toast, Field, Switch, Combobox (search and mentions), Collapsible, Splitter (huddle panel), Presence, Portal.
- `@pandacss/dev` **1.12.1 stable**, not the v2 beta. The beta (2.0.0-beta.18) compiles this config with zero diagnostics but leaves every conditional semantic token unresolved in the output (`background: action.subtle` instead of `var(--colors-action-subtle)`), in `css()` and recipes alike, with or without custom conditions. The same config on 1.12.1 resolves everything. The authoring API is identical, so moving to v2 later is a version bump.
- Fonts: `@fontsource-variable/space-grotesk` added; Outfit and JetBrains Mono are already self-hosted.
- Integration: `@pandacss/vite` next to `@tailwindcss/vite` in `chat/astro.config.mjs` during migration. Panda's cascade layers are renamed `pd_*` so they never merge with Tailwind's `base` and `utilities`. Drop the rename when Tailwind goes.
- Theme: night is the default condition (`:root:not([data-theme=light])`), daylight is `[data-theme=light]`. `preferences/theme.ts` and `ThemeSwitcher.vue` keep working; only the default flips.
- Knip: `styled-system/` is generated; ignore it with a one-line justification and gitignore it.
- `scripts/generate-design-tokens.mjs` should read Panda's generated `tokens.json` and emit the Apple and Android colour sets, collapsing the four divergent teals into one source.

## 5. Migration

1. **Tokens.** Install Panda 1.12, land `panda.config.ts`, generate. Point the token codegen at Panda. Ship Space Grotesk. Nothing visible changes.
2. **Primitives.** Add Ark. Replace tooltips (about 154 `title=` sites), menus (6), dialogs and drawers, toasts (new generic toaster; move the three call toasts onto it), tabs.
3. **People rail and shell.** New `CommunityShell` next to `ChatReadyShell` behind a route flag: header navigation, people rail from `waddles/members.ts` and `presence/*`, context column. The existing timeline, composer and calls are reused inside Rooms.
4. **Rooms.** Occupancy-sorted tiles over `waddles/directory.ts` and room presence; the huddle panel from `CurrentCallPanel` and `CallParticipantsPanel`.
5. **Discussions.** Forum view over MUC threads with accepted answer and kudos. The new wire objects land here.
6. **Members, Events, public page.** Members and Events are re-skins of existing stores; the public page needs the numbers endpoint.
7. **Remove Tailwind and the 11 CSS partials.** Delete the `pd_` prefixes and the `.chat-*` classes.

Steps 1 and 2 are pure engineering with no design risk and can start now.

## 6. Open decisions

1. Wire shapes for accepted answer, kudos and published huddle presence (section 3). Rooms and Discussions cannot finish without them.
2. Whether the penguin mark stays or is replaced (section 2).
3. Whether the public page is served by `website/` (recommended: visitors have no XMPP session) or `chat/`.
4. Library: what a page is (pinned messages, accepted answers, or XEP-0060 items) and what it looks like.
