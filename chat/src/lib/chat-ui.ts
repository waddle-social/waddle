export type AppState = "loading" | "signed-out" | "ready" | "error";
export type AdminTab = "rooms" | "people" | "settings";
export type EditableRole = "member" | "moderator" | "admin";

/** Delivery status for messages sent by the current user. */
export type DeliveryStatus = "sending" | "delivered";

export interface TimelineMessage {
  id: string;
  author: string;
  body: string;
  createdAt: string;
  isSelf: boolean;
  /** Delivery status — only meaningful for self-sent messages. */
  deliveryStatus?: DeliveryStatus;
  /** Whether this message has been edited (XEP-0308). */
  isEdited?: boolean;
  /** Whether this message has been retracted (XEP-0424). */
  isRetracted?: boolean;
  /** Aggregated emoji reactions: emoji -> list of nicks (XEP-0444). */
  reactions?: Record<string, string[]>;
}

export interface CommunityFormData {
  name: string;
  description: string;
  is_public: boolean;
}

export interface ChannelCreateFormData {
  name: string;
  description: string;
  channel_type: string;
  position: number;
}

export interface ChannelEditFormData {
  name: string;
  description: string;
  position: number;
}
