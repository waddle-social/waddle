# PRD-001: Channel-less Chat Experience

**Status:** Draft

**Owner:** Product

**Created:** 2025-09-30

## Overview

Waddle reimagines community chat by removing the rigid channel structure found in Discord and Slack. Instead of pre-defined channels, messages flow into a single stream organized dynamically by conversations, with AI and human tags helping users find what matters to them.

## Problem Statement

Current chat platforms suffer from:

1. **Channel Overload**: Communities create 20-100 channels, overwhelming new members
2. **Fragmented Conversations**: Topics scatter across multiple channels
3. **Missed Context**: Users miss important conversations in channels they don't actively monitor
4. **High Friction**: Creating/managing channels requires admin permissions and organizational overhead
5. **Rigid Structure**: Can't reorganize past conversations without moving messages

## Target Users

### Primary Personas

**1. The Contributor (Developer Communities)**
- Wants to help answer questions
- Needs to see unanswered support requests
- Doesn't want to see social chat
- Example: OSS maintainer in Rawkode Academy community

**2. The Learner (Developer Communities)**
- Posts questions seeking help
- Wants to track their own questions and answers
- Interested in learning discussions
- Example: Junior dev learning Kubernetes

**3. The Social Member (Hobby Communities)**
- Discusses multiple topics (SG-1, Star Wars, board games)
- Wants separate views for different interests
- Values community over specific topics
- Example: Sci-fi fan in general hobby community

**4. The Lurker (All Communities)**
- Reads but rarely posts
- Overwhelmed by notification noise
- Wants to catch up on what matters
- Example: Busy professional in industry community

## Success Metrics

- **Engagement**: 40% increase in message read rate vs. traditional channels
- **Retention**: 30% improvement in 7-day retention for new members
- **Findability**: 80% of users can find relevant conversations without search
- **Onboarding**: New members post first message within 5 minutes
- **Satisfaction**: NPS > 50 for conversation organization

## User Stories

### Core Experience

**As a new member**, I want to see relevant conversations immediately so I don't feel lost.

**Acceptance Criteria:**
- Landing page shows top 5 active conversations
- Each conversation has clear title and participant count
- Can browse conversation history without joining

**As an active member**, I want to post messages without choosing a channel so I can focus on content.

**Acceptance Criteria:**
- Message input is always visible, no channel selection needed
- Can optionally add hashtags for context
- Posted message appears in default view immediately

**As a contributor**, I want to see only support questions so I can efficiently help others.

**Acceptance Criteria:**
- Can create "Support" view filtering by #help tag
- View shows unanswered questions first
- Can mark questions as resolved

### Conversation Discovery

**As a member interested in specific topics**, I want my conversations organized by interest so I'm not overwhelmed.

**Acceptance Criteria:**
- Can create multiple views (e.g., "SG-1 Chat", "Star Wars")
- Each view filters by tags or keywords
- Can switch views with one click

**As an admin**, I want to suggest useful views to members so they understand the feature.

**Acceptance Criteria:**
- Can create shared views (e.g., "Welcome", "Announcements", "Support")
- Shared views appear as suggestions for new members
- Can set descriptions explaining each view's purpose

### Message Tagging

**As a message author**, I want to hint at conversation topics so my message is properly grouped.

**Acceptance Criteria:**
- Can add hashtags like #help, #kubernetes, #sg1
- Hashtags are removed from display text
- See which conversations message was added to

**As a reader**, I want AI to organize messages I don't tag so conversations form naturally.

**Acceptance Criteria:**
- Messages without hashtags get AI-generated tags
- AI groups related messages into conversations
- Can see AI confidence score for tags

## User Experience

### Message Posting

```
┌─────────────────────────────────────────────────┐
│ Waddle: Rawkode Academy                         │
├─────────────────────────────────────────────────┤
│                                                  │
│ Views: [All] [My Questions] [Help Needed] [+]  │
│                                                  │
│ ┌─────────────────────────────────────────────┐│
│ │ Conversation: Getting Started with K8s      ││
│ │ #kubernetes #help #beginner                 ││
│ │ 12 messages • 4 participants • 2h ago       ││
│ │                                             ││
│ │ Alice: How do I deploy my first pod?        ││
│ │ Bob: Here's a simple example...             ││
│ │ Alice: Thanks! That worked.                 ││
│ └─────────────────────────────────────────────┘│
│                                                  │
│ ┌─────────────────────────────────────────────┐│
│ │ Conversation: Stargate SG-1 Rewatch         ││
│ │ #sg1 #tv                                    ││
│ │ 45 messages • 8 participants • 1h ago       ││
│ │                                             ││
│ │ Charlie: Just finished Season 3!            ││
│ │ Dana: Window of Opportunity is the best!    ││
│ └─────────────────────────────────────────────┘│
│                                                  │
├─────────────────────────────────────────────────┤
│ [Type a message... #topic]                      │
└─────────────────────────────────────────────────┘
```

### View Switcher

```
┌──────────────────────────────┐
│ My Views                     │
├──────────────────────────────┤
│ ✓ All Messages          (142)│
│   My Questions          (3)  │
│   Help Needed           (8)  │
│   SG-1 Chat            (45)  │
│   Star Wars            (23)  │
├──────────────────────────────┤
│ Shared Views                 │
├──────────────────────────────┤
│   📢 Announcements      (5)  │
│   👋 Welcome            (12) │
│   🎓 Tutorials          (34) │
├──────────────────────────────┤
│ [+ Create New View]          │
└──────────────────────────────┘
```

### View Creation

```
┌──────────────────────────────────────┐
│ Create New View                      │
├──────────────────────────────────────┤
│ Name: [My Kubernetes Questions___]   │
│                                      │
│ Show messages that:                  │
│ [✓] Include tags: [help] [k8s]      │
│ [✓] Are from me                      │
│ [ ] Exclude tags: [resolved]         │
│                                      │
│ Group by: [Conversation ▼]           │
│ Sort by:  [Newest First ▼]           │
│                                      │
│ [ ] Set as default view              │
│                                      │
│        [Cancel]  [Create View]       │
└──────────────────────────────────────┘
```

## Technical Considerations

- See RFC-001 for detailed technical design
- AI tagging latency must be < 2 seconds
- Conversation creation should feel instant (optimistic UI)
- Views must support complex filter combinations
- Message search needs to work across all conversations

## Out of Scope (V1)

- Voice channel organization (still uses traditional channels)
- Private conversations (DMs work differently)
- Message threading (comes later)
- Conversation merging/splitting
- Advanced AI features (sentiment analysis, summarization)

## Launch Plan

### Phase 1: MVP (Weeks 1-4)
- Basic message posting (no channels)
- Manual hashtag tagging
- Simple "All Messages" view
- Basic conversation grouping by hashtags

### Phase 2: Views (Weeks 5-8)
- User-created views with filters
- Shared views from admins
- View switcher UI
- Saved view preferences

### Phase 3: AI Tagging (Weeks 9-12)
- AI-powered tag generation
- Automatic conversation grouping
- Confidence scores
- Feedback mechanism

### Phase 4: Refinement (Weeks 13-16)
- Advanced filters (date ranges, participants)
- Conversation titles and descriptions
- View templates
- Analytics and insights

## Open Questions

1. **How do we handle announcements?** Should admin posts bypass conversation grouping?
2. **What about voice channels?** Should voice remain channel-based or also go channel-less?
3. **How granular should tags be?** Is #kubernetes too broad? Should we encourage #k8s-networking?
4. **Can users edit AI tags?** Should we allow manual tag correction?
5. **How do we prevent view overload?** Should we limit number of views per user?

## Success Criteria

**Must Have:**
- Users can post messages without selecting channels
- At least 3 useful views available (All, Help, Social)
- AI tags messages with 70%+ accuracy
- Conversations form around related messages
- New users understand system within 5 minutes

**Should Have:**
- Users create personal views for their interests
- Shared views reduce setup time for common use cases
- Search works across all conversations
- Mobile experience is smooth

**Nice to Have:**
- AI suggests views based on usage patterns
- Conversation summaries for catch-up
- Trending conversations surface automatically

## Dependencies

- AI service for message analysis
- Real-time infrastructure for instant updates
- User preferences storage
- Analytics for measuring success

## Risks & Mitigation

**Risk:** Users don't understand channel-less model
**Mitigation:** Strong onboarding, pre-built shared views, clear help docs

**Risk:** AI tags incorrectly, frustrating users
**Mitigation:** Show confidence scores, allow manual override, continuous improvement

**Risk:** Performance degrades with many views
**Mitigation:** Limit views per user, optimize queries, cache results

**Risk:** Community prefers traditional channels
**Mitigation:** Offer "classic mode" with traditional channels as views

## Feedback & Iteration

- Weekly user interviews during alpha
- In-app feedback widget for view suggestions
- Analytics on view creation and usage
- A/B test different default views
- Monitor support tickets for confusion points