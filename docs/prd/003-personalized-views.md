# PRD-003: Personalized Views

**Status:** Draft

**Owner:** Product

**Created:** 2025-09-30

## Overview

Personalized Views allow users to create custom lenses on their Waddle's message stream, filtering and organizing conversations according to their specific needs. Views are the core mechanism that makes channel-less chat work for diverse user personas.

## Problem Statement

In traditional chat platforms:
- **Everyone sees the same structure** (channels) regardless of their role or interests
- **Can't personalize** organization without admin permissions
- **Information overload** from channels you don't care about
- **Context switching** between unrelated topics in same space
- **No saved preferences** for how you want to organize content

Example: A developer community member who contributes help might want a "Support" view showing unanswered questions, while a learner wants "My Questions" showing their posts and replies, and a social member wants "SG-1 Chat" for off-topic discussions.

## Target Users

### Primary Personas

**1. The Multi-Interest Member**
- Participates in multiple conversation topics
- Wants clean separation between interests
- Needs quick switching between contexts
- Example: Discusses both Kubernetes and Stargate in same Waddle

**2. The Role-Based Contributor**
- Has specific goals (help others, learn, moderate)
- Needs filtered view for their tasks
- Wants efficient workflow
- Example: OSS maintainer filtering for issues/PRs

**3. The Overwhelmed Newcomer**
- Joins community with 1000s of messages
- Doesn't know what's relevant
- Needs curated starting point
- Example: New joiner using admin-provided "Welcome" view

**4. The Return Visitor**
- Hasn't checked Waddle in days/weeks
- Wants to catch up efficiently
- Needs "what did I miss" summary
- Example: Busy professional catching up on weekends

## Success Metrics

- **View Creation**: 70% of active users create at least 1 custom view
- **View Usage**: Average 3 view switches per session
- **Engagement**: Users with custom views spend 40% more time in Waddle
- **Onboarding**: New users using shared views have 50% better retention
- **Satisfaction**: 80% of users rate views as "very useful" or "essential"

## User Stories

### View Creation

**As a member**, I want to create a view filtering specific topics so I can focus on what interests me.

**Acceptance Criteria:**
- Click "Create View" from view switcher
- Name the view (e.g., "Kubernetes Help")
- Add filter rules (tags, users, content, time)
- Preview results before saving
- Save and switch to new view immediately

**As a member**, I want to combine multiple filters so I can create precise views.

**Acceptance Criteria:**
- Can add multiple filter rules (AND logic)
- Support different filter types: tags, users, keywords, dates
- Can exclude certain tags or users (NOT logic)
- See live preview of message count matching filters

**As a member**, I want to organize how messages display so the view works for my workflow.

**Acceptance Criteria:**
- Choose grouping: conversation, time, or user
- Choose sort order: newest first, oldest first
- Set as default view for this Waddle
- View preferences persist across devices

### View Discovery

**As a new member**, I want pre-built views so I understand how to use them.

**Acceptance Criteria:**
- See "Shared Views" section with admin-created views
- Each shared view has description explaining purpose
- Can try shared view without saving
- Can copy shared view and customize

**As an admin**, I want to create shared views so new members have starting points.

**Acceptance Criteria:**
- Create view marked as "Official"
- Add description and icon
- Appears in all members' view switcher
- Can track usage stats

### View Management

**As a member**, I want to organize my views so I can find them quickly.

**Acceptance Criteria:**
- Reorder views via drag-and-drop
- Archive unused views (hide but don't delete)
- Duplicate view to create variations
- Export/import view configurations

**As a member**, I want to switch views seamlessly so it doesn't interrupt my flow.

**Acceptance Criteria:**
- View switcher always visible (sidebar or dropdown)
- Show unread count per view
- Keyboard shortcuts for switching
- Remember last active view per Waddle

## User Experience

### View Switcher (Sidebar)

```
┌─────────────────────────────┐
│ RAWKODE ACADEMY             │
├─────────────────────────────┤
│ My Views                    │
│                             │
│ • All Messages        (142) │
│ ✓ My Questions         (3) │
│   Help Needed          (8) │
│   Kubernetes          (23) │
│   SG-1 Chat           (45) │
│   Star Wars           (12) │
│                             │
│ [+ Create View]             │
├─────────────────────────────┤
│ Shared Views                │
│                             │
│   📢 Announcements     (5) │
│   👋 Welcome          (12) │
│   🎓 Tutorials        (34) │
│   💬 General          (89) │
│                             │
└─────────────────────────────┘
```

### View Creation Modal

```
┌──────────────────────────────────────────────┐
│ Create View                                  │
├──────────────────────────────────────────────┤
│ View Name:                                   │
│ [My Kubernetes Questions_____________]      │
│                                              │
│ Filter Messages:                             │
│ ┌──────────────────────────────────────────┐│
│ │ Include tags:                            ││
│ │ [kubernetes] [k8s] [help] [+]           ││
│ │                                          ││
│ │ From users:                              ││
│ │ [@me] [+]                               ││
│ │                                          ││
│ │ Exclude tags:                            ││
│ │ [resolved] [closed] [+]                 ││
│ │                                          ││
│ │ Time range:                              ││
│ │ [ Last 24 hours  ▼]                     ││
│ └──────────────────────────────────────────┘│
│                                              │
│ Display Options:                             │
│ Group by:    [Conversation ▼]               │
│ Sort order:  [Newest First ▼]               │
│                                              │
│ Matching messages: 12                        │
│ [Preview]                                    │
│                                              │
│ [ ] Set as default view                      │
│                                              │
│           [Cancel]  [Create View]            │
└──────────────────────────────────────────────┘
```

### View Preview

```
┌──────────────────────────────────────────────┐
│ Preview: My Kubernetes Questions             │
├──────────────────────────────────────────────┤
│ 12 messages matched                          │
│                                              │
│ ⚠️  This view shows only messages tagged    │
│    #kubernetes, #k8s, #help from you,       │
│    excluding #resolved                       │
│                                              │
│ ┌──────────────────────────────────────────┐│
│ │ You: How do I configure ingress?         ││
│ │ 2 hours ago • #kubernetes #help          ││
│ │ 3 replies                                ││
│ └──────────────────────────────────────────┘│
│                                              │
│ ┌──────────────────────────────────────────┐│
│ │ You: Best practices for secrets?         ││
│ │ 1 day ago • #k8s #help                   ││
│ │ 7 replies                                ││
│ └──────────────────────────────────────────┘│
│                                              │
│         [Back to Edit]  [Save View]          │
└──────────────────────────────────────────────┘
```

### Shared View Creation (Admin)

```
┌──────────────────────────────────────────────┐
│ Create Shared View                           │
├──────────────────────────────────────────────┤
│ View Name:                                   │
│ [Welcome & Getting Started__________]       │
│                                              │
│ Description:                                 │
│ [Everything new members need to know_       │
│  ________________________________           │
│  ________________________________]          │
│                                              │
│ Icon (emoji): [👋_]                         │
│                                              │
│ Filter Messages:                             │
│ ┌──────────────────────────────────────────┐│
│ │ Include tags:                            ││
│ │ [welcome] [intro] [getting-started] [+] ││
│ └──────────────────────────────────────────┘│
│                                              │
│ [✓] Mark as official                         │
│ [✓] Show to new members by default           │
│                                              │
│           [Cancel]  [Create Shared View]     │
└──────────────────────────────────────────────┘
```

## View Types & Examples

### Common View Patterns

#### 1. Role-Based Views

**Support View** (for contributors)
- Include: `#help`, `#question`, `#support`
- Exclude: `#resolved`
- Group by: conversation
- Sort: unanswered first

**Moderation View** (for moderators)
- Include: `#report`, `#flag`
- Group by: time
- Sort: newest first

#### 2. Topic-Based Views

**Kubernetes Discussion**
- Include: `#kubernetes`, `#k8s`, `#container`
- Group by: conversation
- Sort: newest first

**Off-Topic Chat**
- Include: `#sg1`, `#starwars`, `#games`, `#random`
- Exclude: `#technical`
- Group by: time

#### 3. Personal Views

**My Activity**
- From: me
- Group by: time
- Sort: newest first

**Mentions & Replies**
- Mentions: me
- OR replies to: my messages
- Group by: conversation
- Sort: newest first

#### 4. Discovery Views

**Trending**
- Messages with 5+ reactions
- Last 7 days
- Group by: conversation
- Sort: most reactions

**New Conversations**
- Conversations started in last 24 hours
- Group by: conversation
- Sort: newest first

## Filter Types

### Tag Filters
- **Includes**: Message must have these tags
- **Excludes**: Message must NOT have these tags
- **Any of**: Message has at least one tag
- Example: `includes:[help] excludes:[resolved]`

### User Filters
- **From**: Messages from specific users
- **Not from**: Exclude specific users
- **Mentions**: Messages mentioning user
- Example: `from:[@me] OR mentions:[@me]`

### Content Filters
- **Contains**: Text search in message content
- **Matches regex**: Advanced pattern matching
- Example: `contains:"kubernetes" OR contains:"k8s"`

### Time Filters
- **Last X hours/days/weeks**
- **Before date**
- **After date**
- **Between dates**
- Example: `last 7 days`

### Conversation Filters
- **Has replies**: Messages with responses
- **Unanswered**: No replies yet
- **Active**: Recent activity
- Example: `unanswered AND includes:[help]`

## Technical Considerations

- See ADR-004 for storage architecture
- Views stored in per-user D1 database
- Queries must be efficient (< 100ms)
- Support complex filter combinations
- Filter execution order matters for performance
- Cache view results for common queries

## Out of Scope (V1)

- AI-suggested views based on behavior
- Collaborative views (shared between users)
- View templates marketplace
- Advanced filters (sentiment, reactions, attachments)
- View analytics and insights
- Scheduled views (daily digest)
- View permissions (admin-only views)

## Launch Plan

### Phase 1: Basic Views (Weeks 1-3)
- View creation UI
- Tag and user filters
- Manual switching between views
- View list management

### Phase 2: Shared Views (Weeks 4-6)
- Admin creates shared views
- New member onboarding with suggested views
- View descriptions and icons
- Copy shared view to personal

### Phase 3: Advanced Filters (Weeks 7-9)
- Content search filters
- Time range filters
- Conversation state filters
- Complex filter logic (AND/OR/NOT)

### Phase 4: View Management (Weeks 10-12)
- Reorder and organize views
- Archive unused views
- Export/import configurations
- Usage analytics

## Open Questions

1. **How many views is too many?** Should we limit to 10 per user?
2. **Should views work across Waddles?** Or only per-Waddle?
3. **Can views be public?** Should users share view configs?
4. **How do we handle view migrations?** If filter schema changes?
5. **Should we have view templates?** Pre-built configs users can start from?

## Success Criteria

**Must Have:**
- Users can create unlimited personal views
- Tag, user, and basic content filters work
- View switching is instant (< 100ms)
- Shared views appear for all members
- View preferences persist across sessions

**Should Have:**
- Preview before saving view
- Unread counts per view
- Keyboard shortcuts for switching
- View reordering

**Nice to Have:**
- AI view suggestions
- View analytics
- Collaborative views
- View templates

## Dependencies

- Per-user D1 database (ADR-004)
- Channel-less conversation model (RFC-001)
- GraphQL views schema
- Efficient query execution
- User preferences storage

## Risks & Mitigation

**Risk:** View queries too slow with complex filters
**Mitigation:** Query optimization, caching, index strategy

**Risk:** Users create too many unused views
**Mitigation:** Archive feature, suggested cleanup, limits

**Risk:** New users don't understand views
**Mitigation:** Strong onboarding, shared view templates, tooltips

**Risk:** Filter syntax too complex
**Mitigation:** Visual filter builder, common templates, help docs