/** XEP-0030: Service discovery for waddles and channels. */
import type { Agent } from "stanza";
import type { DataFormField, DiscoInfoResult } from "stanza/protocol";
import { normalizeChannelType } from "@/lib/channel-types";
import type { DiscoveredChannel, DiscoveredSpace, DiscoveredTopology } from "./types";
import { barePeerJid, jidDomain } from "./jid";

const NS_FORUMS_0 = "urn:xmpp:forums:0";
const NS_BOOKMARKS_1 = "urn:xmpp:bookmarks:1";
const NS_MUC = "http://jabber.org/protocol/muc";
const NS_SPACES_0 = "urn:xmpp:spaces:0";
const FIELD_FORUM_MODE = "muc#roomconfig_forum";
type DiscoveryRole = "owner" | "admin" | "moderator" | "member" | null;
type PubsubItem = {
  id?: string;
  content?: {
    itemType?: string;
    name?: string;
    autojoin?: boolean;
    autoJoin?: boolean;
  };
};
type PubsubAgent = Agent & {
  getItems(jid: string, node: string, opts?: { max?: number }): Promise<{ items?: PubsubItem[] }>;
};

function fieldStringValue(field: DataFormField | undefined): string | null {
  if (!field || field.value === undefined) return null;
  const rawValue = Array.isArray(field.value) ? field.value[0] : field.value;
  if (rawValue === undefined || rawValue === null) return null;
  return String(rawValue).trim() || null;
}

function metadataField(infoResult: DiscoInfoResult, name: string): string | null {
  for (const form of infoResult.extensions ?? []) {
    if (!form.fields) continue;
    const field = form.fields.find((f) => f.name === name);
    const value = fieldStringValue(field);
    if (value) return value;
  }
  return null;
}

function parseBooleanValue(value: string | null): boolean | null {
  if (!value) return null;
  const normalized = value.toLowerCase();
  if (["1", "true", "yes"].includes(normalized)) return true;
  if (["0", "false", "no"].includes(normalized)) return false;
  return null;
}

function parseChannelType(infoResult: DiscoInfoResult) {
  const hasForumFeature = infoResult.features?.includes(NS_FORUMS_0);
  const forumField = parseBooleanValue(metadataField(infoResult, FIELD_FORUM_MODE));
  return normalizeChannelType(hasForumFeature || forumField ? "forum" : "text");
}

function parseDiscoveryRole(value: string | null): DiscoveryRole {
  switch (value) {
    case "owner":
      return "owner";
    case "admin":
    case "publisher":
      return "admin";
    case "moderator":
      return "moderator";
    case "member":
      return "member";
    default:
      return null;
  }
}

export function spacesServiceDomain(jid: string): string {
  return `spaces.${jidDomain(jid)}`;
}

export function mucServiceDomain(jid: string): string {
  return `muc.${jidDomain(jid)}`;
}

async function discoverComponentServices(
  xmpp: Agent,
  domain: string,
  jid: string,
): Promise<{ muc: string; spaces: string }> {
  const fallback = {
    muc: mucServiceDomain(jid),
    spaces: spacesServiceDomain(jid),
  };

  try {
    const response = await xmpp.getDiscoItems(domain);
    const candidates = response.items?.map((item) => item.jid).filter((value): value is string => !!value) ?? [];
    const infos = await Promise.all(
      candidates.map(async (serviceJid) => {
        try {
          return { serviceJid, info: await xmpp.getDiscoInfo(serviceJid) };
        } catch {
          return { serviceJid, info: null };
        }
      }),
    );
    return infos.reduce((services, candidate) => {
      const features = candidate.info?.features ?? [];
      const identities = candidate.info?.identities ?? [];
      if (features.includes(NS_MUC) || identities.some((identity) => identity.category === "conference")) {
        services.muc = candidate.serviceJid;
      }
      if (features.includes(NS_SPACES_0) || identities.some((identity) => identity.category === "pubsub")) {
        services.spaces = candidate.serviceJid;
      }
      return services;
    }, fallback);
  } catch {
    return fallback;
  }
}

function channelFromDiscoItem(
  item: { jid?: string; name?: string; node?: string },
  position: number,
  extra: Partial<DiscoveredChannel> = {},
): DiscoveredChannel | null {
  const itemJid = item.jid ?? "";
  const channelId = barePeerJid(itemJid).split("@")[0] ?? "";
  if (!channelId) return null;
  return {
    id: channelId,
    name: item.name ?? channelId,
    jid: item.jid,
    channelType: "text",
    position,
    ...extra,
  };
}

function channelFromSpaceItem(
  item: PubsubItem,
  position: number,
  extra: Partial<DiscoveredChannel> = {},
): DiscoveredChannel | null {
  const itemJid = item.id ?? "";
  const channelId = barePeerJid(itemJid).split("@")[0] ?? "";
  if (!channelId) return null;
  const content = item.content;
  if (content?.itemType && content.itemType !== NS_BOOKMARKS_1) return null;
  return {
    id: channelId,
    name: content?.name ?? channelId,
    jid: itemJid,
    channelType: "text",
    position,
    ...extra,
  };
}

async function hydrateChannelType(
  xmpp: Agent,
  channel: DiscoveredChannel,
): Promise<DiscoveredChannel> {
  if (!channel.jid) return channel;
  try {
    const info = await xmpp.getDiscoInfo(channel.jid);
    return {
      ...channel,
      channelType: parseChannelType(info),
    };
  } catch {
    return channel;
  }
}

export async function discoverChannels(
  xmpp: Agent,
  jid: string,
): Promise<DiscoveredChannel[]> {
  const spacesDomain = spacesServiceDomain(jid);
  const spacesResponse = await xmpp.getDiscoItems(spacesDomain);
  const spaceNode = spacesResponse.items?.find((item) => item.node)?.node;

  if (!spaceNode) return [];

  const response = await (xmpp as PubsubAgent).getItems(spacesDomain, spaceNode, { max: 500 });
  const discovered = (response.items ?? [])
    .map((item, position) => channelFromSpaceItem(item, position))
    .filter((channel): channel is DiscoveredChannel => !!channel);

  const hydrated = await Promise.all(discovered.map((channel) => hydrateChannelType(xmpp, channel)));
  return hydrated.map((room, position) => ({
    id: room.id,
    name: room.name,
    channelType: room.channelType,
    position,
  }));
}

export async function discoverTopology(
  xmpp: Agent,
  jid: string,
): Promise<DiscoveredTopology> {
  const domain = jidDomain(jid);
  const services = await discoverComponentServices(xmpp, domain, jid);
  const mucDomain = services.muc;
  const spacesDomain = services.spaces;

  const roomsByJid = new Map<string, DiscoveredChannel>();
  const spaces: DiscoveredSpace[] = [];
  let serverRole: DiscoveryRole = null;

  try {
    const serverInfo = await xmpp.getDiscoInfo(domain);
    serverRole = parseDiscoveryRole(metadataField(serverInfo, "waddle#server_affiliation"));
  } catch {
    serverRole = null;
  }

  try {
    const mucResponse = await xmpp.getDiscoItems(mucDomain);
    (mucResponse.items ?? []).forEach((item, position) => {
      const channel = channelFromDiscoItem(item, position, { standalone: true });
      if (channel?.jid) roomsByJid.set(barePeerJid(channel.jid), channel);
    });
  } catch {
    // Empty or not-yet-initialized MUC services are a valid first-run state.
  }

  try {
    const spacesResponse = await xmpp.getDiscoItems(spacesDomain);
    for (const [spacePosition, item] of (spacesResponse.items ?? []).entries()) {
      const spaceId = item.node ?? barePeerJid(item.jid ?? "").split("@")[0] ?? `space-${spacePosition}`;
      if (!spaceId) continue;
      let spaceRole = serverRole;
      if (item.node) {
        try {
          const spaceInfo = await xmpp.getDiscoInfo(spacesDomain, item.node);
          spaceRole =
            parseDiscoveryRole(metadataField(spaceInfo, "pubsub#affiliation")) ??
            serverRole;
        } catch {
          spaceRole = serverRole;
        }
      }
      spaces.push({
        id: spaceId,
        name: item.name ?? spaceId,
        role: spaceRole,
      });

      if (!item.node) continue;
      const response = await (xmpp as PubsubAgent).getItems(spacesDomain, item.node, { max: 500 });
      (response.items ?? []).forEach((child, position) => {
        const channel = channelFromSpaceItem(child, position, {
          spaceId,
          standalone: false,
        });
        if (!channel?.jid) return;
        const key = barePeerJid(channel.jid);
        roomsByJid.set(key, {
          ...(roomsByJid.get(key) ?? channel),
          ...channel,
        });
      });
    }
  } catch {
    // No spaces is the expected state on a fresh deployment.
  }

  const rooms = await Promise.all(
    [...roomsByJid.values()]
      .sort((a, b) => a.position - b.position || a.name.localeCompare(b.name))
      .map((room, position) => hydrateChannelType(xmpp, { ...room, position })),
  );

  return {
    spaces,
    rooms,
    serverRole,
    services,
  };
}
