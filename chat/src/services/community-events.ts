/**
 * xCal community events composable. Wraps the wasm bridge's
 * `xcal_items` / `xcal_publish` calls behind a reactive surface that
 * sorts upcoming-first and exposes typed event + RRULE state to the
 * UI.
 *
 * Recurrence expansion (computing each occurrence date from a master
 * event + RRULE) is deferred for now — the UI renders the master
 * event with its RRULE summary ("Weekly on Fridays · 10 occurrences").
 * Per-instance overrides will come back via RECURRENCE-ID in a
 * follow-up.
 */
import { computed, ref, type Ref } from "vue";
import {
  groupEventsWithRsvps,
  sortEventsUpcomingFirst,
  type BrowserXmppClient,
  type CommunityEvent,
  type CommunityEventInput,
  type PartStat,
} from "@/lib/xmpp-client";
import {
  expandInstances,
  groupEventsWithOverrides,
} from "@/lib/xmpp/event-expansion";

export function useCommunityEvents(
  xmppClient: Ref<BrowserXmppClient | null>,
  options: {
    communityJid: Ref<string | null>;
    pageSize?: number;
  },
) {
  const events = ref<CommunityEvent[]>([]);
  const isLoading = ref(false);
  const isPosting = ref(false);
  const error = ref<string | null>(null);
  const pageSize = options.pageSize ?? 200;
  let fetchRequestId = 0;

  const sortedEvents = computed(() => {
    const merged = groupEventsWithRsvps(events.value);
    const expanded: CommunityEvent[] = [];
    for (const group of groupEventsWithOverrides(merged)) {
      if (group.master.rrule) {
        expanded.push(...expandInstances(group));
      } else {
        expanded.push(group.master);
      }
    }
    return sortEventsUpcomingFirst(expanded);
  });

  async function refresh(): Promise<boolean> {
    const client = xmppClient.value;
    const jid = options.communityJid.value;
    if (!client || !jid) {
      events.value = [];
      return false;
    }
    const requestId = ++fetchRequestId;
    isLoading.value = true;
    error.value = null;
    try {
      const fetched = await client.fetchCommunityEvents(jid, pageSize);
      if (requestId !== fetchRequestId || client !== xmppClient.value) return false;
      events.value = fetched;
      return true;
    } catch (err) {
      if (requestId === fetchRequestId) {
        error.value = err instanceof Error ? err.message : String(err);
      }
      return false;
    } finally {
      if (requestId === fetchRequestId) {
        isLoading.value = false;
      }
    }
  }

  async function post(input: CommunityEventInput): Promise<CommunityEvent | null> {
    const client = xmppClient.value;
    const jid = options.communityJid.value;
    if (!client || !jid) return null;
    if (!input.summary.trim()) return null;
    isPosting.value = true;
    error.value = null;
    try {
      const event = await client.publishCommunityEvent(jid, input);
      events.value = [event, ...events.value.filter((e) => e.id !== event.id)];
      return event;
    } catch (err) {
      error.value = err instanceof Error ? err.message : String(err);
      return null;
    } finally {
      isPosting.value = false;
    }
  }

  /**
   * Publish (or update) this session's RSVP for an event. Server
   * persists a sibling pubsub item; we optimistically fold the new
   * attendee into the local master and rely on refresh() for the
   * authoritative state from peers.
   */
  async function rsvp(
    masterUid: string,
    selfLocalpart: string,
    selfBareJid: string,
    partstat: PartStat,
  ): Promise<boolean> {
    const client = xmppClient.value;
    const jid = options.communityJid.value;
    if (!client || !jid) return false;
    if (!masterUid || !selfLocalpart || !selfBareJid) return false;
    try {
      await client.rsvpCommunityEvent(jid, masterUid, selfLocalpart, selfBareJid, partstat);
      const rsvpId = `${masterUid}-rsvp-${selfLocalpart}`;
      const placeholder: CommunityEvent = {
        id: rsvpId,
        uid: masterUid,
        summary: "",
        attendees: [{ uri: `xmpp:${selfBareJid}`, partstat }],
      };
      events.value = [
        placeholder,
        ...events.value.filter((e) => e.id !== rsvpId),
      ];
      return true;
    } catch (err) {
      error.value = err instanceof Error ? err.message : String(err);
      return false;
    }
  }

  function clear() {
    fetchRequestId += 1;
    events.value = [];
    error.value = null;
    isLoading.value = false;
    isPosting.value = false;
  }

  return {
    events: sortedEvents,
    isLoading,
    isPosting,
    error,
    refresh,
    post,
    rsvp,
    clear,
  };
}
