# PRD-002: Web Content Integration

**Status:** Draft

**Owner:** Product

**Created:** 2025-09-30

## Overview

Waddle integrates external web content (RSS feeds, YouTube videos, GitHub activity) directly into conversations, enabling communities to discuss content from across the web without leaving the platform.

## Problem Statement

Communities discuss external content constantly, but current platforms make this friction-filled:

1. **Manual Sharing**: Someone must notice new content and manually share it
2. **Lost Context**: Shared links lack rich previews and context
3. **Fragmented Discussion**: Comments happen on YouTube/Twitter, not in community
4. **No History**: Can't see what content was previously discussed
5. **Missing Notifications**: Members miss content they care about

Example: Rawkode Academy publishes weekly videos, but relies on manual Discord posts to notify community.

## Target Users

### Primary Personas

**1. The Content Creator (Rawkode Academy)**
- Publishes videos, blog posts, releases
- Wants community discussing their content
- Needs automatic cross-posting
- Example: Rawkode posts YouTube video → auto-shared to Waddle

**2. The Community Manager**
- Curates interesting content for community
- Wants automated sharing of relevant feeds
- Needs to reduce manual work
- Example: Sets up RSS feeds for industry news

**3. The Open Source Maintainer**
- Wants community aware of releases and PRs
- Needs GitHub activity in community chat
- Wants to discuss changes in context
- Example: New release → posted to Waddle with discussion

**4. The Community Member**
- Wants to discover and discuss content
- Doesn't want to check multiple platforms
- Needs relevant content surfaced automatically
- Example: Sees new video in Waddle, watches and discusses

## Success Metrics

- **Adoption**: 60% of Waddles configure at least 1 integration
- **Engagement**: 2x more comments on integrated content vs. manual shares
- **Content Discovery**: 40% of members discover content via Waddle first
- **Retention**: Waddles with integrations have 25% better retention
- **Automation**: 80% reduction in manual content sharing by admins

## User Stories

### Integration Setup

**As a Waddle admin**, I want to connect RSS feeds so members automatically see new posts.

**Acceptance Criteria:**
- Can add RSS feed URL in settings
- Choose tags to apply to posts
- See preview of recent feed items before enabling
- Receive test post to verify configuration

**As a content creator**, I want my YouTube channel connected so my community sees new videos immediately.

**Acceptance Criteria:**
- Connect via YouTube channel URL or ID
- Filter by video type (videos, shorts, livestreams)
- Custom message format (with/without description)
- Videos appear as rich embeds with thumbnails

**As an OSS maintainer**, I want GitHub releases posted to my Waddle so users stay informed.

**Acceptance Criteria:**
- Connect via repository name (owner/repo)
- Select events (releases, PRs, issues)
- Configure which events to post
- GitHub posts link back to original

### Content Consumption

**As a member**, I want integrated content to feel native so I don't leave Waddle to engage.

**Acceptance Criteria:**
- RSS posts show title, summary, and link
- YouTube posts have embedded player (optional)
- GitHub posts show status badges and metadata
- Can react and comment without leaving Waddle

**As a member with specific interests**, I want to filter integrated content so I'm not overwhelmed.

**Acceptance Criteria:**
- Integrated posts have tags (e.g., #rss, #youtube, #github)
- Can create views excluding integration content
- Can create views showing only specific integrations
- Integration posts grouped into conversations like other messages

### Management

**As an admin**, I want to monitor integrations so I know they're working.

**Acceptance Criteria:**
- Dashboard shows last sync time per integration
- See error messages if sync fails
- Can manually trigger sync/refresh
- Disable integration without deleting configuration

**As an admin**, I want to configure posting frequency so we don't spam the community.

**Acceptance Criteria:**
- Set minimum time between posts (e.g., max 1 per hour)
- Batch multiple items into single post
- Quiet hours (don't post at night)
- Maximum posts per day limit

## User Experience

### Integration Setup UI

```
┌──────────────────────────────────────────────┐
│ Integrations for Rawkode Academy            │
├──────────────────────────────────────────────┤
│                                              │
│ Active Integrations (3)                      │
│                                              │
│ ┌──────────────────────────────────────────┐│
│ │ 📡 Rawkode Academy Blog                  ││
│ │ RSS • Last sync: 2 hours ago             ││
│ │ 24 posts • #blog #tutorials              ││
│ │                                          ││
│ │ [Edit] [Pause] [Delete]                  ││
│ └──────────────────────────────────────────┘│
│                                              │
│ ┌──────────────────────────────────────────┐│
│ │ 🎥 Rawkode Academy YouTube               ││
│ │ YouTube • Last sync: 1 hour ago          ││
│ │ 156 videos • #video #tutorial            ││
│ │                                          ││
│ │ [Edit] [Pause] [Delete]                  ││
│ └──────────────────────────────────────────┘│
│                                              │
│ ┌──────────────────────────────────────────┐│
│ │ 🔧 pulumi/pulumi                         ││
│ │ GitHub • Last sync: 30 minutes ago       ││
│ │ 12 releases • #releases                  ││
│ │                                          ││
│ │ [Edit] [Pause] [Delete]                  ││
│ └──────────────────────────────────────────┘│
│                                              │
│ [+ Add Integration]                          │
└──────────────────────────────────────────────┘
```

### Add Integration Flow

```
┌──────────────────────────────────────────┐
│ Add Integration                          │
├──────────────────────────────────────────┤
│ Choose Type:                             │
│                                          │
│ ┌──────────┐ ┌──────────┐ ┌──────────┐ │
│ │   📡     │ │   🎥     │ │   🔧     │ │
│ │   RSS    │ │ YouTube  │ │  GitHub  │ │
│ │          │ │          │ │          │ │
│ └──────────┘ └──────────┘ └──────────┘ │
└──────────────────────────────────────────┘

┌──────────────────────────────────────────┐
│ Add RSS Feed                             │
├──────────────────────────────────────────┤
│ Feed URL:                                │
│ [https://rawkode.academy/feed.xml____] │
│                                          │
│ Display Name:                            │
│ [Rawkode Academy Blog_______________]  │
│                                          │
│ Tags (optional):                         │
│ [blog] [tutorials] [+]                  │
│                                          │
│ Options:                                 │
│ [✓] Include post description             │
│ [ ] Include full content                 │
│ [ ] Post as bot user                     │
│                                          │
│ Sync Frequency:                          │
│ [ ] Every 5 minutes                      │
│ [✓] Every 15 minutes                     │
│ [ ] Every hour                           │
│ [ ] Every 6 hours                        │
│                                          │
│       [Cancel]  [Test Feed]  [Create]    │
└──────────────────────────────────────────┘
```

### Integration Post Format

```
┌────────────────────────────────────────────┐
│ 🤖 Rawkode Academy Bot · 2 hours ago       │
├────────────────────────────────────────────┤
│ 📡 New post from Rawkode Academy Blog      │
│                                            │
│ **Getting Started with Pulumi on AWS**    │
│                                            │
│ Learn how to provision AWS infrastructure  │
│ using Pulumi's TypeScript SDK. We'll      │
│ deploy a complete web application...      │
│                                            │
│ 🔗 https://rawkode.academy/pulumi-aws     │
│                                            │
│ #blog #tutorials #aws #pulumi              │
├────────────────────────────────────────────┤
│ 👍 5  💬 3                                 │
└────────────────────────────────────────────┘
```

## Supported Integrations (V1)

### RSS Feeds
- **Any RSS 2.0 or Atom feed**
- Sync frequency: 5 min - 6 hours
- Rich preview with title, description, image
- Custom tags per feed
- Filter by keywords (optional)

### YouTube
- **Channels, playlists**
- Filter by content type (video, short, livestream)
- Embedded player option
- New uploads only (not all history)
- Sync every 30 minutes

### GitHub
- **Public repositories only (V1)**
- Events: releases, PRs, issues, discussions
- Webhook support (instant) + polling fallback
- Rich preview with status, labels, author
- Link to GitHub for full context

## Technical Considerations

- See RFC-002 for detailed implementation
- Polling frequency must respect external API limits
- Use webhooks where available (GitHub)
- Handle rate limiting gracefully
- Store integration state for recovery
- Deduplicate posts (don't re-post same item)

## Out of Scope (V1)

- Twitter/X integration
- Slack/Discord message imports
- Calendar integrations (Google Calendar, Outlook)
- Email newsletters
- Podcast feeds (audio embeds)
- Private repository access (GitHub)
- Two-way sync (post from Waddle to GitHub)

## Launch Plan

### Phase 1: RSS Only (Weeks 1-3)
- RSS feed integration only
- Manual sync trigger
- Basic formatting
- Tag support

### Phase 2: YouTube (Weeks 4-6)
- YouTube channel integration
- Embedded player
- Content type filtering
- Thumbnail previews

### Phase 3: GitHub (Weeks 7-9)
- GitHub repository integration
- Webhook support
- Event filtering
- Rich metadata

### Phase 4: Polish (Weeks 10-12)
- Improved formatting
- Batch posting
- Quiet hours
- Analytics dashboard

## Open Questions

1. **How do we handle high-volume feeds?** Should we limit posts per day?
2. **Should integrations post to specific channels?** Or always to main stream?
3. **Can users mute specific integrations?** Even if admin enabled them?
4. **How do we show integration attribution?** Bot user or special badge?
5. **Should we support webhooks for RSS?** Zapier, IFTTT integration?

## Success Criteria

**Must Have:**
- RSS, YouTube, GitHub integrations working
- Rich previews for all content types
- Admins can configure in under 2 minutes
- Posts feel native to Waddle
- No duplicate posts

**Should Have:**
- Webhook support for GitHub (instant)
- Embedded YouTube player
- Error handling and retry logic
- Integration analytics

**Nice to Have:**
- AI summaries of linked content
- Batch posting for high-volume feeds
- User-level integration muting
- Two-way sync (future)

## Dependencies

- Scheduled workers for polling
- Event bus for posting
- Bot user system
- URL preview service
- External API credentials storage

## Risks & Mitigation

**Risk:** External APIs change or rate limit us
**Mitigation:** Polling fallback, graceful degradation, error monitoring

**Risk:** Integration spam overwhelms conversation
**Mitigation:** Posting limits, quiet hours, user muting

**Risk:** Security concerns with external content
**Mitigation:** Content sanitization, safe rendering, URL validation

**Risk:** Performance impact from frequent polling
**Mitigation:** Efficient scheduling, caching, batch processing