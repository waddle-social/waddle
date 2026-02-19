export interface DirectMessageLike {
  from: string;
  to: string;
  body: string;
  timestamp?: string | null;
  messageType?: string;
}

export interface ConversationSummaryLike {
  jid: string;
  title: string;
  preview: string;
  updatedAt: string | null;
  unreadCount: number;
}

interface BuildConversationUpdateOptions<TSummary extends ConversationSummaryLike> {
  selfJid: string;
  activeConversationJid: string | null;
  existing: TSummary | null;
  titleFromJid: (jid: string) => string;
}

export interface ConversationMessageUpdate<TSummary extends ConversationSummaryLike> {
  peerJid: string;
  isIncoming: boolean;
  summary: TSummary;
}

export function bareJid(value: string): string {
  return value.split('/')[0] || value;
}

export function normalizeConversationJid(value: string | null | undefined): string | null {
  if (!value) return null;
  const normalized = bareJid(value.trim());
  return normalized.length > 0 ? normalized : null;
}

export function peerJidForDirectMessage(message: DirectMessageLike, selfJid: string): string | null {
  if (message.messageType === 'groupchat') return null;

  const self = normalizeConversationJid(selfJid);
  const from = normalizeConversationJid(message.from);
  const to = normalizeConversationJid(message.to);
  if (!self || !from || !to) return null;

  const peerJid = from === self ? to : from;
  if (peerJid === self) return null;
  return peerJid;
}

export function buildConversationUpdateFromMessage<TSummary extends ConversationSummaryLike>(
  message: DirectMessageLike,
  options: BuildConversationUpdateOptions<TSummary>,
): ConversationMessageUpdate<TSummary> | null {
  const peerJid = peerJidForDirectMessage(message, options.selfJid);
  if (!peerJid) return null;

  const existing = options.existing;
  const from = normalizeConversationJid(message.from);
  const active = normalizeConversationJid(options.activeConversationJid);
  const isIncoming = from === peerJid;
  const shouldIncrementUnread = isIncoming && active !== peerJid;

  const summary = {
    jid: peerJid,
    title: existing?.title || options.titleFromJid(peerJid),
    preview: message.body.trim() || existing?.preview || '',
    updatedAt: message.timestamp ?? existing?.updatedAt ?? null,
    unreadCount: shouldIncrementUnread
      ? (existing?.unreadCount ?? 0) + 1
      : (existing?.unreadCount ?? 0),
  } as TSummary;

  return {
    peerJid,
    isIncoming,
    summary,
  };
}

export function clearConversationUnreadCount<TSummary extends ConversationSummaryLike>(
  conversations: TSummary[],
  conversationJid: string,
): TSummary[] {
  const target = normalizeConversationJid(conversationJid);
  if (!target) return conversations;

  return conversations.map((item) =>
    item.jid === target && item.unreadCount !== 0
      ? ({ ...item, unreadCount: 0 })
      : item,
  );
}
