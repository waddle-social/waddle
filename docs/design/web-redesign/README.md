# Waddle Web redesign: community-first concepts on Panda CSS + Ark UI

Status: concept. Nothing here changes `chat/` yet.

Canvas with the visual concepts (six artboards, clickable between screens):
https://claude.ai/artifact/NLk2yGmdN3GxP5niioC6bG

Files in this folder:

- `README.md` — this brief: diagnosis, brand direction, three concepts, stack decision, migration plan.
- `panda.config.ts` — proposed token, semantic-token and recipe config. Verified with `panda codegen` and `panda cssgen` on `@pandacss/dev@1.12.1` (clean) and `2.0.0-beta.18` (compiles, but see the token-resolution regression in section 4).
- `prototypes/MemberMenu.vue` — an Ark UI `Menu` wired to the `menu` and `avatar` slot recipes, the pattern for replacing hand-rolled menus.

## 1. Where the web app is today

Facts from `chat/` on `main` (see `docs/planning/2026-07-refactoring-plan.md` and `.impeccable.md` for prior direction):

- Astro 6 + Vue 3 islands, Tailwind v4 (CSS-first, `@theme inline` in `src/styles/global/tokens.css`), 4,095 lines of hand-written global CSS across 11 partials, plus ~3,000 inline utility class strings in components.
- 120 Vue components, all hand-rolled. No headless library. Six ad hoc `role="menu"` implementations, about 154 native `title=` tooltips, dialogs via a 39-line `AppDialog`, no generic toast system (only the call toasts).
- Theme "Aether": one teal (`oklch(0.63 0.13 184)`), frosted glass panels, `light-dark()` everywhere. The brand teal is defined four different ways across web (`#00a292`), the PWA manifest (`#00b4a0`), Android (`#14B8A6`) and the generated Apple accent.
- The logo's orange beak (`#ff7f21`) is the only warm colour in the brand and appears nowhere in the app.
- Information architecture is channel-first: rail → "Spaces" list → channel → timeline. Community surfaces (Feed, Events, Stories, Threads, Unread) are rows inside the channel sidebar. Members exist only as a header facepile and a modal. The multi-community switcher in `WaddlesSidebar.vue` is called with `waddles=[]`, so it is inert. There is no invite UI, no discovery UI, and no onboarding beyond login.
- The product docs already say what "community-first" means: PRD-001 (channel-less stream, conversations organised by tags and lenses), PRD-003 (personalised views per persona), and the presence ADR. The current UI does not express any of it.

Conclusion: the redesign is an information-architecture change first and a visual change second. Restyling the channel-first shell would waste the effort.

## 2. Brand direction: "Rookery"

A rookery is where penguins gather. The brand keeps the penguin and adds warmth.

| Element | Decision | Why |
| --- | --- | --- |
| Mark | Keep the speech-bubble penguin as is | It is already good and already everywhere (favicons, Apple, Android). |
| Wordmark | `waddle.social` in Fredoka 600, orange dot | The website already does this; the app should match it. |
| Action colour | Teal 700 `#0b7a6f` (light) / Teal 300 `#6fd3c7` (dark) | Continuity with Aether. The lighter light-mode teal used today fails AA for white text on buttons (3.2:1); 700 passes (5.2:1). |
| Warm colour | Beak orange `#ff7f21` for fills with ink text, `#b04a06` for text on light | Reserved for people and live moments: new members, mentions, huddles, "say hi". Never for destructive actions. |
| Neutrals | Ink scale from the logo navy `#121f2b`; ice ground `#f2f6f7` for light; optional sand ground `#f8f4ec` for the warm concept | One neutral family instead of three near-identical oklch greys. |
| Type | Fredoka for display (greetings, community names, empty states), Outfit for UI and messages, JetBrains Mono for code | Fredoka is playful without being childish and matches the website. Outfit is already loaded. |
| Glass | Retire frosted-glass panels and the ambient body gradient | They cost paint time, fight contrast, and read as "utility app". Flat surfaces with one shadow scale instead. |
| Mascot | The penguin greets (home hero), fills silence (empty states), and marks your own account. Nowhere else. | Same rule the iteration-20 screenshots established; now written down. |

Principles (also on the brand artboard):

1. The community is the home, not the channel.
2. Warm by default, dense on demand. Density is a user setting (`data-density`), not a brand trait.
3. The penguin has a job.

## 3. Three concept directions

All three share the tokens in `panda.config.ts` and the same Ark UI primitives. They differ in what the primary object on screen is.

### Concept A: Colony (community home first) — recommended

Artboards: `A-Colony`, `Mobile`.

- Left rail is communities (real multi-community switching, using the inert `waddles` prop), not "Spaces vs DMs" toggles.
- The community sidebar leads with **Views** (PRD-003 lenses: Support, My questions, Announcements, user-defined), then Rooms, then the next event.
- Landing page is a **community home**: time-of-day greeting with the penguin, "Needs an answer" (unanswered questions, the contributor persona from PRD-001), "Happening now" (live huddles, hot threads), and a "New this week" welcome strip with a one-tap "Say hi".
- A persistent **Around now** column with presence and rich status (mood/activity/tune already published via PEP). The app has the data; it just never shows it.
- Top-level tabs: Home, Stream, People, Events, About.
- Phone: bottom tabs (Home, Stream, People, Events, Inbox) replace the drawer.

Why recommended: it delivers the PRD intent with the least protocol work. Every surface shown is backed by an existing store or service in `chat/src` (see the survey table in section 6). Risk: it needs a real definition of "unanswered" (no reply from anyone other than the author within N hours) and a way to mark resolved, both of which are XEP-0444 reactions or a `urn:waddle:*` PEP annotation, to be decided.

### Concept B: Floe (stream first)

Artboard: `B-Floe`.

- No channels in the primary navigation. One stream of **conversation cards** (title, tags, state, participants, last activity), filtered by lenses and tags. This is PRD-001 literally.
- Warm sand ground, Bricolage Grotesque headings, coral-orange as the primary action, teal secondary. Reads more like a social product than a chat client.
- Stories, events and the live huddle sit in a right column.

Strength: the strongest expression of channel-less chat and the most distinctive look. Cost: rooms still exist on the wire (MUC) and moderators still need them; hiding them entirely creates a second mental model. Recommended use: take the conversation card and the Needs-you / Active / Resolved segmentation into Concept A's Stream tab.

### Concept C: Huddle (people first)

Artboard: `C-Huddle`.

- People are the primary navigation: "In a huddle", "Around", "Away". Rooms are tiles sorted by who is in them, with a "quiet" state that invites rather than shames.
- Dark ink ground, Space Grotesk headings, teal glow, orange for live. Voice huddle panel is always present when you are in one.

Strength: makes presence and calls (28 call components, LiveKit) the centre, which nothing else on the market does well for communities. Cost: bad for the lurker persona and for large communities where 1,000 offline members is noise. Recommended use: take the people rail and the huddle panel into Concept A's People tab and in-call state.

## 4. Stack decision: Panda CSS + Ark UI

### What is true on 2026-09-26

- `@pandacss/dev` stable is 1.12.1. **v2 is a beta** (`2.0.0-beta.18`, tag `beta`). v2 keeps the authoring API (`css()`, `cva`, `sva`, patterns, tokens, conditions) and rewrites the compiler in Rust on Oxc. It is ESM-only and needs Node 22+; `chat/` already satisfies both.
- Known v2 beta caveats from the maintainers' migration guide: the PostCSS plugin is experimental (use `@pandacss/vite@beta`), Astro `@pandacss/studio` is gone, hooks moved to `plugins`, and there is an open regression in nested `&.class { & .child }` selectors inside `sva` recipes.
- **Found while validating `panda.config.ts` (blocking for this app):** on `2.0.0-beta.18`, semantic tokens whose value is conditional (`{ _light: …, _dark: … }`) are emitted as CSS variables but their references are left unresolved in generated rules. `bg: "action.subtle"` compiles to `background: action.subtle` instead of `var(--colors-action-subtle)`, in both `css()` and recipes; plain semantic tokens (`bg: "warm"`) resolve. Removing the custom light/dark conditions does not change it. The same config on `1.12.1` resolves every reference. A light-and-dark app cannot ship on the beta until this is fixed upstream; it should be reported with the `panda debug` dump from the spike.
- `@ark-ui/vue` is stable at 5.39.x and supports Vue 3.5. It ships the primitives this app hand-rolls today: Menu, Popover, Tooltip, Dialog, Tabs, Combobox (mention and slash popovers), Avatar, Toast, Switch, Field, Select, Editable, Collapsible, Splitter (thread pane), TreeView (spaces and rooms), Presence (animate in/out), Portal.
- `@park-ui/panda-preset` (0.43.x) is a Panda preset plus Ark-styled components. It is a reasonable reference, not a dependency: Waddle's tokens are its own, and Park's preset would bring a second colour system.

### Recommendation

Adopt Panda now on **stable 1.12.x**, not the v2 beta. The authoring API is identical, so the move to v2 is a version bump once the conditional-token regression above is fixed; the Rust compiler's speed is not worth blocking a redesign on. The token and recipe layer is the part that pays off (one source of truth, typed, tree-shaken, and exportable to Apple and Android through the existing `scripts/generate-design-tokens.mjs`); the compiler version is an implementation detail behind it. Track the v2 beta in CI as an allowed-failure job so the switch is a one-line change when it is ready.

Integration points:

- `chat/astro.config.mjs`: add `@pandacss/vite` next to `@tailwindcss/vite` during migration; remove Tailwind at the end.
- Cascade layers: Tailwind v4 and Panda both declare `base` and `utilities`. The config renames Panda's layers to `pd_*` so the two never merge while both are installed. Drop the rename when Tailwind is removed.
- Theme switching: conditions `_light` and `_dark` map onto the existing `data-theme` attribute and system fallback in `AppLayout.astro`, so `preferences/theme.ts` and `ThemeSwitcher.vue` keep working unchanged.
- Knip: add `styled-system/` to the ignore list with a one-line justification (generated output), and gitignore it.
- Fonts: Fredoka via `@fontsource-variable/fredoka` (self-hosted like Outfit today); no Google Fonts at runtime.

## 5. Migration plan

Phased so every step ships behind the existing UI and CI stays green.

1. **Tokens (1 PR).** Install Panda, land `panda.config.ts`, generate `styled-system/`. Extend `scripts/generate-design-tokens.mjs` to read the Panda token JSON instead of `--primary` from `tokens.css`, so the four divergent teals collapse into one. Nothing visible changes.
2. **Primitives (1 PR each).** Add `@ark-ui/vue`. Replace, in order: Tooltip (154 `title=` sites, mostly mechanical), Menu (6 sites), Dialog and Drawer (`AppDialog`, `AppDrawer`, `ConfirmDialog`, `ChatMobileDrawers`), Toast (new generic toaster; move the three call toasts onto it), Tabs (admin already planned Ark Tabs in `docs/superpowers/plans/2026-05-17-admin-v2-implementation.md`).
3. **Shell (Concept A).** New `CommunityShell` next to `ChatReadyShell` behind a route flag: community rail, Views-first sidebar, community home, Around-now column. Timeline, composer and calls are reused as is.
4. **Stream tab.** Conversation cards and lens filters from Concept B on top of `channels/inbox.ts`, `channels/threads.ts` and `lib/unread-overview-state.ts`. Requires the "unanswered / resolved" annotation decision.
5. **People tab and huddles.** People rail from Concept C on `waddles/members.ts` and `presence/*`; huddle panel from `CurrentCallPanel` and `CallParticipantsPanel`.
6. **Remove Tailwind and the 11 CSS partials.** Delete `pd_` layer prefixes. Delete `.chat-*` classes as the last consumers move.

Each step is independently shippable and reversible. Steps 1 and 2 are pure engineering with no design risk and could start now.

## 6. Feature backing for the concepts

Every surface drawn on the artboards maps to something that already exists in `chat/src`:

| Concept surface | Existing store or service |
| --- | --- |
| Community switcher | `waddles/directory.ts` (`SpaceSummary`, `is_public`), disco topology |
| Views / lenses | PRD-003; `lib/unread-overview-state.ts`, `channels/inbox.ts`; TODO-ISSUES #368 saved views |
| Needs an answer | `channels/threads.ts`, `channels/messages.ts` plus a new "answered" annotation |
| Around now + rich status | `presence/*`, `publishMood` / `publishActivity` / `publishTune` (XEP-0107/0108/0118) |
| Huddles | `lib/calls/*`, `CurrentCallPanel`, `CallParticipantsPanel` |
| Events | `services/community-events.ts`, `EventsPane` (recurrence, RSVP, iCal) |
| Stories | `services/stories.ts`, `StoryReaderDialog` |
| New members / say hi | `waddles/members.ts`, roster; needs the mediated MUC invite UI (server side done, TODO-ISSUES #1248) |
| Hot threads | `lib/thread-*.ts`, thread chip participants (iteration 26) |

## 7. Open decisions

1. "Unanswered" and "resolved" semantics: XEP-0444 reaction, XEP-0422 fastening, or a `urn:waddle:*` PEP node. This gates the Support view.
2. Whether the community switcher shows every space the server hosts or only bookmarked ones (XEP-0402 bookmarks, TODO-ISSUES #752/#961).
3. When to move from Panda 1.12.x to v2: after the conditional semantic-token regression is fixed upstream and the CI allowed-failure job on `@beta` goes green.
4. Whether to keep glass anywhere (for example call overlays). The concepts remove it.
