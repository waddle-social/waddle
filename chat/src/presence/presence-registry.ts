// Tracks a contact's presence per resource (full JID) and collapses it to
// the single Show to render, via most-available-wins (ADR-010). Inbound
// presence is per-resource; this is where several resources of one contact
// become one dot. It also carries each resource's XEP-0319 idle instant so an
// away dot can render the idle age ("away, idle 20m").

import { mostAvailableShow, type RenderedShow } from "./most-available";

interface ResourcePresence {
  show: RenderedShow;
  /** XEP-0319 idle instant (epoch ms) for this resource, if it sent one. */
  idleSince?: number;
}

export class PresenceRegistry {
  private byBare = new Map<string, Map<string, ResourcePresence>>();

  /** Record one resource's Show (+ optional idle) and return the rendered Show. */
  apply(bareJid: string, resource: string, show: RenderedShow, idleSince?: number): RenderedShow {
    const resources = this.byBare.get(bareJid) ?? new Map<string, ResourcePresence>();
    // Offline means that resource is gone — drop it rather than retain a
    // dead entry. XMPP hands out a fresh resource id per reconnect, so
    // keeping offline resources would grow this map without bound.
    if (show === "offline") {
      resources.delete(resource);
    } else {
      resources.set(resource, idleSince === undefined ? { show } : { show, idleSince });
    }
    if (resources.size === 0) {
      this.byBare.delete(bareJid);
    } else {
      this.byBare.set(bareJid, resources);
    }
    return this.renderedFor(bareJid);
  }

  renderedFor(bareJid: string): RenderedShow {
    const resources = this.byBare.get(bareJid);
    if (!resources || resources.size === 0) return "offline";
    return mostAvailableShow([...resources.values()].map((r) => r.show));
  }

  /**
   * The idle instant (epoch ms) to render for the contact, or `undefined` when
   * none applies. Only the resources carrying the rendered Show count, and only
   * an away/xa Show carries idle — Available reads as "here now". With several
   * idle resources the most-recently-active (latest instant) wins, mirroring
   * the dot the contact actually shows.
   */
  renderedIdleFor(bareJid: string): number | undefined {
    const rendered = this.renderedFor(bareJid);
    if (rendered !== "away" && rendered !== "xa") return undefined;
    const resources = this.byBare.get(bareJid);
    if (!resources) return undefined;
    let latest: number | undefined;
    for (const r of resources.values()) {
      if (r.show === rendered && r.idleSince !== undefined) {
        latest = latest === undefined ? r.idleSince : Math.max(latest, r.idleSince);
      }
    }
    return latest;
  }

  /**
   * Forget all tracked presence. Call on logout and on a fresh reconnect:
   * across a disconnect we may never observe a resource's `unavailable`, so
   * its entry would otherwise linger and distort the aggregated Show.
   */
  clear(): void {
    this.byBare.clear();
  }
}
