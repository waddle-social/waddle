/** XEP-0030: Service discovery for waddles and channels. */
import type { Agent } from "stanza";
import type { DataFormField, DiscoInfoResult } from "stanza/protocol";
import { normalizeChannelType } from "@/lib/channel-types";
import type { DiscoveredChannel, DiscoveredSpace, DiscoveredTopology } from "./types";
import { barePeerJid, jidDomain } from "./jid";

const NS_FORUMS_0 = "urn:xmpp:forums:0";
const FIELD_FORUM_MODE = "muc#roomconfig_forum";

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

function spacesServiceDomain(jid: string): string {
  return `spaces.${jidDomain(jid)}`;
}

function mucServiceDomain(jid: string): string {
  return `muc.${jidDomain(jid)}`;
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

async function discoverCommandCapability(
  xmpp: Agent,
  serverJid: string,
  node: string,
): Promise<boolean> {
  try {
    const response = await xmpp.getDiscoItems(serverJid, "http://jabber.org/protocol/commands");
    return (response.items ?? []).some((item) => item.node === node);
  } catch {
    return false;
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

  const response = await xmpp.getDiscoItems(spacesDomain, spaceNode);
  const discovered = (response.items ?? [])
    .map((item, position) => channelFromDiscoItem(item, position))
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
  const serverJid = jidDomain(jid);
  const mucDomain = mucServiceDomain(jid);
  const spacesDomain = spacesServiceDomain(jid);

  const roomsByJid = new Map<string, DiscoveredChannel>();
  const spaces: DiscoveredSpace[] = [];

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
      spaces.push({
        id: spaceId,
        name: item.name ?? spaceId,
      });

      if (!item.node) continue;
      const response = await xmpp.getDiscoItems(spacesDomain, item.node);
      (response.items ?? []).forEach((child, position) => {
        const channel = channelFromDiscoItem(child, position, {
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

  const [canCreateMuc, canCreateSpace] = await Promise.all([
    discoverCommandCapability(xmpp, serverJid, "waddle:create-muc"),
    discoverCommandCapability(xmpp, serverJid, "waddle:create-space"),
  ]);

  return {
    spaces,
    rooms,
    canCreateMuc: canCreateMuc || await discoverCommandCapability(xmpp, serverJid, "waddle:create-channel"),
    canCreateSpace,
  };
}
