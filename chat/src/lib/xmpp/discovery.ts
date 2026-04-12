/** XEP-0030: Service discovery for waddles and channels. */
import type { Agent } from "stanza";
import type { DiscoveredChannel, DiscoveredWaddle } from "./types";
import { jidDomain } from "./jid";

function parseAccessModel(
  infoResult: { extensions?: Array<{ fields?: Array<{ name?: string; value?: unknown }> }> },
): "open" | "whitelist" | null {
  if (!infoResult.extensions) return null;

  for (const form of infoResult.extensions) {
    if (!form.fields) continue;
    const field = form.fields.find((f) => f.name === "pubsub#access_model");
    if (!field) continue;
    const value = (typeof field.value === "string" ? field.value : String(field.value ?? ""))
      .trim().toLowerCase();
    if (value === "open" || value === "whitelist") return value;
  }
  return null;
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
        return { ...waddle, isPublic: parseAccessModel(info) !== "whitelist" };
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
