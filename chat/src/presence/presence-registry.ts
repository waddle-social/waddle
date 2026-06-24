// Tracks a contact's presence per resource (full JID) and collapses it to
// the single Show to render, via most-available-wins (ADR-010). Inbound
// presence is per-resource; this is where several resources of one contact
// become one dot.

import { mostAvailableShow, type RenderedShow } from "./most-available";

export class PresenceRegistry {
  private byBare = new Map<string, Map<string, RenderedShow>>();

  /** Record one resource's Show and return the contact's rendered Show. */
  apply(bareJid: string, resource: string, show: RenderedShow): RenderedShow {
    const resources = this.byBare.get(bareJid) ?? new Map<string, RenderedShow>();
    // Offline means that resource is gone — drop it rather than retain a
    // dead entry. XMPP hands out a fresh resource id per reconnect, so
    // keeping offline resources would grow this map without bound.
    if (show === "offline") {
      resources.delete(resource);
    } else {
      resources.set(resource, show);
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
    return mostAvailableShow([...resources.values()]);
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
