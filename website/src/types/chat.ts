export type TagType = 'dm' | 'rss' | 'broadcast' | 'private' | 'public' | 'bluesky';

export interface TagMeta {
  id: string;
  name: string;
  type: TagType;
  unread: number;
}

export type TagDictionary = Record<string, TagMeta[]>;

export interface Reply {
  id: number;
  author: string;
  content: string;
  time: string;
  replyCount?: number;
  replies?: Reply[];
  tags?: string[];
}

export interface Message extends Reply {
  waddleId: string;
}
