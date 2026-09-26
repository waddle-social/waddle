# Hearth: a new brand and design system for Waddle as a community platform

Status: concept. Nothing here changes `chat/` yet. This replaces the earlier "Rookery" evolution of the Aether theme; the direction is now a clean sheet.

Canvas with the full system (eight artboards, clickable between screens):
https://claude.ai/artifact/NLk2yGmdN3GxP5niioC6bG

Files in this folder:

- `README.md` — this brief: the idea, brand, tokens, product surfaces, stack, migration.
- `panda.config.ts` — the token, semantic-token, text-style and recipe config. Verified with `panda codegen` and `panda cssgen` on `@pandacss/dev@1.12.1`.
- `prototypes/MemberMenu.vue` — an Ark UI `Menu` wired to the `menu` and `avatar` slot recipes.

## 1. The idea

Waddle is a community platform. Chat is one room in the house, not the house.

A community is people taking turns keeping each other warm. Emperor penguins survive the winter in a huddle, rotating so no one stays on the cold edge. That is the brand idea and the product idea: the platform's job is to notice who is on the edge (an unanswered question, a new member nobody has greeted) and move them toward the centre.

Three consequences for the design:

1. **Editorial, not utility.** Serif headlines, numbered sections, hairline rules, generous measure. It reads like a well-run magazine, not a terminal. This is the sharpest way to stop looking like Slack or Discord.
2. **Warm ground, one ember.** Two grounds (paper, night), one hot colour for action, gold for recognition, moss for resolved. No glass, no gradients, no ambient glow.
3. **People before messages.** Every screen shows who before what. Recognition (kudos, accepted answer, helper, host) is a first-class object with its own tokens.

## 2. Brand

| Element | Decision |
| --- | --- |
| Mark | New: the huddle. Three rounded pills pressed together, each with a small ember dot (the beak). Reads at 16px, works one-colour, and is drawn as inline SVG, so it takes the current text colour. |
| Wordmark | `waddle` in Fraunces 600, optical size 144, soft 60. Lowercase. |
| App icon | Ember ground, paper huddle. Dark variant: night ground, paper huddle, ember dots. |
| Display type | Fraunces (variable: optical size, weight, soft). Headlines, community names, titles, big numbers. |
| Body type | Instrument Sans. Large x-height, slightly narrow; dense lists stay readable at 14px. |
| Mono | IBM Plex Mono. Kickers, timestamps, counts, code. |
| Shape | Radii 4 / 6 / 8 / 10 / pill. 1px hairlines everywhere. No drop shadows on the page; one shadow token for overlays. |
| Presence | A shape, not a colour: filled dot is here, ring is away, slashed dot is do not disturb, dashed ring is offline. Colour-blind safe and it does not compete with ember. |
| Voice | Names the person and the act, never the metric alone. "Three questions are waiting on someone like you." "sol asked their first question. Welcome them." |

### Colour

| Token | Light (paper) | Dark (night) | Role |
| --- | --- | --- | --- |
| `bg` | `#f6efe4` | `#1c1530` | ground |
| `surface` | `#fffbf4` | `#2a2142` | cards, inputs |
| `surface.2` | `#efe6d6` | `#3a3057` | hover, code inline |
| `border` | `#e3d9c8` | `#4a4066` | hairlines |
| `rule` | `#1c1530` | `#f3ecdf` | the strong rule under headers |
| `fg` | `#1c1530` | `#f3ecdf` | text |
| `fg.muted` | `#5d5670` (6.0:1) | `#b3aac6` (7.1:1) | secondary text |
| `ember` | `#c43e17` (white text 5.2:1) | `#ff6a3d` (ink text 6.4:1) | the one action colour |
| `ember.text` | `#a3320f` (6.0:1 on paper) | `#ff6a3d` | links that act, active nav |
| `gold` | `#e9b44c` | `#e9b44c` | kudos, helper |
| `gold.text` | `#8a5a0a` | `#e9b44c` | |
| `moss` | `#2f6b3f` | `#7fc98f` | answered, accepted |
| `link` | `#5b52c7` | `#a9a3ea` | links inside body copy only |
| `danger` | `#b3261e` | `#e0554b` | |

All hex values are in `panda.config.ts`; the semantic layer flips with `_light` / `_dark` conditions bound to the existing `data-theme` attribute.

### Type scale (text styles in the config)

`display` 52 / `headline` 36 / `title` 22 (all Fraunces) · `lead` 17 / `body` 15 / `control` 14 (Instrument Sans) · `kicker` 11 mono uppercase, tracked 0.08em · `stat` 36 Fraunces at optical size 72 for big numbers.

## 3. Product surfaces (the artboards)

The information architecture is a top navigation, not a chat rail: **Home · Discussions · Events · Members · Library · Chat**. Chat carries an unread count and is the last item.

| Screen | What it is for | Backed by |
| --- | --- | --- |
| **Community home** | Time-of-day greeting; "01 — Needs someone like you" (unanswered, numbered); "02 — Latest discussions" with answered state and participants; this week's events; new members with a Welcome action; most helpful this month. | `channels/threads.ts`, `channels/inbox.ts`, `services/community-events.ts`, `waddles/members.ts`, kudos (new) |
| **Discussion** | Forum-style page: Space and type kicker, serif title, author with role tag, body with code, **accepted answer** block in moss, replies, participants, related Library pages, and an anchor to the live room. | MUC threads (`channels/threads.ts`), XEP-0444 reactions for kudos, accepted answer (new annotation) |
| **Public community page** | What a stranger sees: name, description, honest numbers (members, % answered, median first reply, events/month), what people are talking about, spaces, hosts, how it works, Join. | `waddles/directory.ts` (`is_public`), disco, MAM stats (new) |
| **Members** | Directory as cards with role, bio, status and kudos; filters Here now / Helpers / New / Everyone; a profile panel with pronouns, JID, badges, rich status, local time, stats, Message and Give kudos. | vCard4 (`client-vcard.ts`), PEP mood/activity/tune, presence, `waddles/members.ts` |
| **Events** | Month grid with today marked, an event card with RSVP (Going / Maybe / Can't), Add to calendar, Join the room, iCal subscribe, coming up list. | `services/community-events.ts`, `EventsPane` recurrence and RSVP, `lib/calendar-feed-url.ts` |
| **Phone** | Same home, bottom tabs Home / Discuss / Events / People / Chat. | |
| **Tokens and components** | Every Ark primitive the app needs, in both grounds, with the recipe that styles it. | |

**Library** (pinned answers, docs, recordings) is named in the navigation but not drawn; it is the accepted-answer store plus pinned messages, and the design of a document page is a follow-up.

### New objects the platform needs on the wire

These do not exist in `chat/src` today and gate the community screens:

- **Accepted answer / resolved** on a discussion. Recommend a `urn:waddle:answer:0` fastening (XEP-0422) referencing the accepted message, set by the author or a host.
- **Kudos** on a message or a person. XEP-0444 reactions with a reserved emoji would work for messages; per-person totals need a PEP node or a server-side count.
- **Roles** Host / Helper / Member map onto MUC affiliations (owner, admin, member) plus hats (XEP-0317) for "Helper".
- **Public stats** for the join page (percent answered, median first reply) are MAM-derived and should be computed server side.

## 4. Stack: Panda CSS + Ark UI

- `@ark-ui/vue` 5.39.x, Vue ≥ 3.5. Primitives used across the boards: Tabs (as primary navigation), SegmentGroup (filters), Menu, Popover, Tooltip, Dialog, Avatar, Toast, Field, Switch, Combobox (mention/search), Editable (topic), Collapsible, Splitter, TreeView (spaces), Presence, Portal.
- `@pandacss/dev` **1.12.1 stable**, not the v2 beta. The v2 beta (2.0.0-beta.18) compiles this config with zero diagnostics but leaves every conditional semantic token unresolved in the output (`background: ember.subtle` instead of `var(--colors-ember-subtle)`), in both `css()` and recipes. Reproduced with and without the custom light/dark conditions; the same config on 1.12.1 resolves everything. The authoring API is identical, so moving to v2 later is a version bump. Report upstream with the `panda debug` dump.
- Fonts self-hosted through Fontsource: `@fontsource-variable/fraunces`, `@fontsource-variable/instrument-sans`, `@fontsource/ibm-plex-mono`. No runtime Google Fonts.
- Integration: `@pandacss/vite` next to `@tailwindcss/vite` in `chat/astro.config.mjs` during migration. Panda's cascade layers are renamed `pd_*` in the config so they never merge with Tailwind's `base` and `utilities`. Drop the rename when Tailwind goes.
- Theme switching: `_light` / `_dark` conditions map onto the existing `data-theme` attribute and system fallback in `AppLayout.astro`, so `preferences/theme.ts` and `ThemeSwitcher.vue` keep working.
- Knip: `styled-system/` is generated; add it to the ignore list with a one-line justification and gitignore it.
- The token codegen script (`scripts/generate-design-tokens.mjs`) should read Panda's generated `tokens.json` and emit the Apple and Android colour sets, which collapses the four divergent teals today into one source.

## 5. Migration

1. **Tokens.** Install Panda 1.12, land `panda.config.ts`, generate. Update the token codegen to source from Panda. Ship the new fonts. Nothing visible changes.
2. **Primitives.** Add Ark. Replace tooltips (about 154 `title=` sites), menus (6), dialogs and drawers, toasts (new generic toaster; move the three call toasts onto it), tabs.
3. **Shell.** New `CommunityShell` next to `ChatReadyShell` behind a route flag: top navigation, community home. The existing timeline, composer and calls become the Chat tab as is.
4. **Discussions.** Forum view over MUC threads with accepted answer and kudos. This is where the new wire objects land.
5. **Members, Events, public page.** Members and Events are re-skins of existing stores; the public page needs the stats endpoint.
6. **Remove Tailwind and the 11 CSS partials.** Delete the `pd_` prefixes and the `.chat-*` classes.

Steps 1 and 2 are pure engineering with no design risk and can start now.

## 6. Open decisions

1. Wire shapes for accepted answer and kudos (section 3). The Discussions step cannot start without them.
2. Whether Fraunces is acceptable for the wordmark on Apple and Android, where the apps currently use the system font. The mark itself has no dependency.
3. Whether the public community page is served by `website/` (Astro static) or `chat/` (needs the XMPP session). Recommendation: `website/`, reading the stats from the server over a small public endpoint, since visitors have no XMPP session.
4. Library: what a document page looks like and whether it is pinned messages, accepted answers, or a real page type (XEP-0060 PubSub items).
