export type ContentType = 'chat' | 'event' | 'link' | 'person' | 'hangout' | 'message';

export type ContentCategory = 
  | 'Support'
  | 'Help Wanted'
  | 'Videos'
  | 'Music'
  | 'Movies'
  | 'General'
  | 'Tech'
  | 'Gaming';

export type ViewType = 'feed' | 'chat' | 'events' | 'people' | 'links' | 'hangouts' | 'messages';

export type LayoutMode = 'feed' | 'grid' | 'timeline' | 'kanban';

export interface BaseContent {
  id: string;
  type: ContentType;
  userId: string;
  username: string;
  avatar?: string;
  timestamp: number;
  category?: ContentCategory;
  tags?: string[];
  reactions?: Record<string, number>;
  userReaction?: string;
  isBookmarked?: boolean;
  visibility: 'public' | 'friends' | 'private';
}

export interface ChatMessage extends BaseContent {
  type: 'chat';
  content: string;
  isEdited?: boolean;
  editedAt?: number;
  replyTo?: string;
  mentions?: string[];
  attachments?: Attachment[];
}

export interface Event extends BaseContent {
  type: 'event';
  title: string;
  description: string;
  startTime: number;
  endTime: number;
  location?: {
    type: 'physical' | 'virtual';
    address?: string;
    virtualUrl?: string;
  };
  rsvpStatus: 'going' | 'maybe' | 'not_going' | 'none';
  attendeeCount: number;
  maxAttendees?: number;
  isRecurring?: boolean;
  recurringPattern?: RecurringPattern;
}

export interface Link extends BaseContent {
  type: 'link';
  title: string;
  url: string;
  description?: string;
  thumbnail?: string;
  domain: string;
  votes: number;
  userVote?: 'up' | 'down';
  commentCount: number;
  isNSFW?: boolean;
}

export interface Person extends BaseContent {
  type: 'person';
  displayName: string;
  bio?: string;
  location?: string;
  website?: string;
  joinedAt: number;
  followerCount: number;
  followingCount: number;
  isFollowing: boolean;
  mutualConnections: number;
  skills?: string[];
  interests?: string[];
  isOnline: boolean;
  lastSeen?: number;
}

export interface Hangout extends BaseContent {
  type: 'hangout';
  title: string;
  description?: string;
  hangoutType: 'voice' | 'video' | 'stream' | 'watch_party';
  isLive: boolean;
  participantCount: number;
  maxParticipants?: number;
  isPublic: boolean;
  requiresApproval?: boolean;
  streamUrl?: string;
  thumbnailUrl?: string;
  duration?: number;
  scheduledFor?: number;
}

export interface DirectMessage extends BaseContent {
  type: 'message';
  content: string;
  conversationId: string;
  isGroupMessage: boolean;
  participants: string[];
  isRead: boolean;
  deliveredAt?: number;
  readAt?: number;
  attachments?: Attachment[];
}

export type ContentItem = ChatMessage | Event | Link | Person | Hangout | DirectMessage;

export interface Attachment {
  id: string;
  type: 'image' | 'video' | 'audio' | 'document' | 'link';
  url: string;
  fileName?: string;
  fileSize?: number;
  mimeType?: string;
  metadata?: Record<string, any>;
}

export interface RecurringPattern {
  frequency: 'daily' | 'weekly' | 'monthly' | 'yearly';
  interval: number;
  daysOfWeek?: number[];
  dayOfMonth?: number;
  endDate?: number;
  occurrences?: number;
}

export interface CustomView {
  id: string;
  name: string;
  icon?: string;
  contentTypes: ContentType[];
  filters: {
    categories: ContentCategory[];
    users: string[];
    timeRange: {
      start?: number;
      end?: number;
      preset?: 'hour' | 'day' | 'week' | 'month' | 'year';
    };
    keywords: string[];
    tags: string[];
  };
  layout: LayoutMode;
  sortBy: 'timestamp' | 'votes' | 'reactions' | 'relevance';
  sortOrder: 'asc' | 'desc';
  isDefault?: boolean;
  isPublic?: boolean;
}

export interface FeedItem {
  content: ContentItem;
  score: number;
  reason?: string;
  groupKey?: string;
  isPromoted?: boolean;
  isTrending?: boolean;
}

export interface ContentMetrics {
  views: number;
  engagement: number;
  shareCount: number;
  bookmarkCount: number;
  commentCount: number;
  trendingScore: number;
}

export interface User {
  id: string;
  username: string;
  displayName: string;
  email: string;
  avatar?: string;
  bio?: string;
  location?: string;
  website?: string;
  joinedAt: number;
  isVerified?: boolean;
  preferences: UserPreferences;
}

export interface UserPreferences {
  defaultView: ViewType;
  customViews: CustomView[];
  notifications: {
    mentions: boolean;
    directMessages: boolean;
    events: boolean;
    following: boolean;
    trending: boolean;
  };
  privacy: {
    showOnlineStatus: boolean;
    allowDirectMessages: 'everyone' | 'following' | 'none';
    profileVisibility: 'public' | 'friends' | 'private';
  };
  content: {
    autoplayVideos: boolean;
    showNSFW: boolean;
    compactMode: boolean;
  };
}

export interface CustomFeed {
  id: string;
  name: string;
  emoji?: string;
  color: string;
  contentTypes: ContentType[];
  keywords: string[];
  filters: {
    contentTypes: ContentType[];
    keywords: string[];
    categories?: ContentCategory[];
    users?: string[];
    timeRange?: {
      start?: number;
      end?: number;
      preset?: 'hour' | 'day' | 'week' | 'month' | 'year';
    };
  };
  unreadCount: number;
  isActive?: boolean;
  sortBy?: 'timestamp' | 'votes' | 'reactions' | 'relevance';
  sortOrder?: 'asc' | 'desc';
}

export interface Notification {
  id: string;
  userId: string;
  type: 'mention' | 'message' | 'event' | 'follow' | 'reaction' | 'system';
  title: string;
  message: string;
  contentId?: string;
  contentType?: ContentType;
  fromUserId?: string;
  fromUsername?: string;
  timestamp: number;
  isRead: boolean;
  actionUrl?: string;
}