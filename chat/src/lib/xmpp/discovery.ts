/** XEP-0030: Service discovery for waddles and channels. */
import type { Agent } from "stanza";
import type { DataFormField, DiscoInfoResult } from "stanza/protocol";
import { normalizeChannelType } from "@/lib/channel-types";
import type { DiscoveredChannel } from "./types";
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
    .map((item, position) => {
      const itemJid = item.jid ?? "";
      const channelId = barePeerJid(itemJid).split("@")[0] ?? "";
      return {
        id: channelId,
        name: item.name ?? channelId,
        jid: item.jid,
        position,
      };
    })
    .filter((channel) => channel.id);

  return Promise.all(
    discovered.map(async (channel) => {
      if (!channel.jid) {
         return {
           id: channel.id,
           name: channel.name,
           channelType: "text",
           position: channel.position,
         } satisfies DiscoveredChannel;
       }

       try {
        const info = await xmpp.getDiscoInfo(channel.jid);
        return {
           id: channel.id,
           name: channel.name,
           channelType: parseChannelType(info),
           position: channel.position,
         } satisfies DiscoveredChannel;
       } catch {
         return {
           id: channel.id,
           name: channel.name,
           channelType: "text",
           position: channel.position,
         } satisfies DiscoveredChannel;
       }
     }),
   );
}
