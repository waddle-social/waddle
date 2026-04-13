/** XEP-0030: Service discovery for waddles and channels. */
import type { Agent } from "stanza";
import type { DataFormField, DiscoInfoResult } from "stanza/protocol";
import type { DiscoveredChannel, DiscoveredWaddle } from "./types";
import { jidDomain } from "./jid";

function fieldStringValue(field: DataFormField | undefined): string | null {
  if (!field || field.value === undefined || Array.isArray(field.value)) return null;
  return String(field.value).trim() || null;
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

function parseAccessModel(infoResult: DiscoInfoResult): "open" | "whitelist" | null {
  const value = metadataField(infoResult, "pubsub#access_model")?.toLowerCase();
  if (value) {
    if (value === "open" || value === "whitelist") return value;
  }
  return null;
}

function parseWaddleName(infoResult: DiscoInfoResult): string | null {
  return (
    metadataField(infoResult, "pubsub#title") ??
    infoResult.identities.find((identity) => identity.name)?.name?.trim() ??
    null
  );
}

export async function discoverWaddles(xmpp: Agent, jid: string): Promise<DiscoveredWaddle[]> {
  const spacesDomain = `spaces.${jidDomain(jid)}`;
  const response = await xmpp.getDiscoItems(spacesDomain, "");

  const discovered = (response.items ?? [])
    .map((item) => ({ id: item.node ?? "", name: item.name ?? item.node ?? "" }))
    .filter((w) => w.id);

  return Promise.all(
    discovered.map(async (waddle) => {
      try {
        const info = await xmpp.getDiscoInfo(spacesDomain, waddle.id);
        return {
          ...waddle,
          name: parseWaddleName(info) ?? waddle.name,
          isPublic: parseAccessModel(info) !== "whitelist",
        };
      } catch {
        return { ...waddle, isPublic: true };
      }
    }),
  );
}

export async function discoverChannels(
  xmpp: Agent,
  jid: string,
  waddleId: string,
): Promise<DiscoveredChannel[]> {
  const spacesDomain = `spaces.${jidDomain(jid)}`;
  const response = await xmpp.getDiscoItems(spacesDomain, waddleId);

  const prefix = `${waddleId}_`;
  return (response.items ?? [])
    .map((item) => {
      const itemJid = item.jid ?? "";
      const localPart = itemJid.split("@")[0] ?? "";
      const channelId = localPart.startsWith(prefix) ? localPart.slice(prefix.length) : localPart;
      return { id: channelId, name: item.name ?? channelId };
    })
    .filter((c) => c.id);
}
