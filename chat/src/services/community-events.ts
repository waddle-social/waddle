/**
 * xCal community events composable. Wraps the wasm bridge's
 * `xcal_items` / `xcal_publish` calls behind a reactive surface that
 * sorts upcoming-first and exposes typed event + RRULE state to the
 * UI.
 *
 * Recurrence expansion and RECURRENCE-ID overrides are derived from the
 * xCal masters here so the UI can render concrete upcoming instances while
 * edit/cancel actions still target the stored PubSub master item.
 */
import { computed, ref, type Ref } from "vue";
import {
  groupEventsWithRsvps,
  calendarDateKey,
  compareCalendarDateValues,
  sortEventsUpcomingFirst,
  type BrowserXmppClient,
  type CalendarDateValue,
  type CommunityEvent,
  type CommunityEventInput,
  type PartStat,
} from "@/lib/xmpp-client";
import {
  expandInstances,
  groupEventsWithOverrides,
  type CalendarMaster,
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

  const calendarItems = computed<CalendarMaster[]>(() => {
    const merged = groupEventsWithRsvps(events.value);
    return groupEventsWithOverrides(merged);
  });

  const sortedEvents = computed(() => {
    const expanded: CommunityEvent[] = [];
    for (const group of calendarItems.value) {
      if (group.master.rrule) {
        expanded.push(...expandInstances(group));
      } else {
        expanded.push(group.master);
      }
    }
    return sortEventsUpcomingFirst(expanded);
  });

  /**
   * Look up the unexpanded master event behind an expanded instance
   * (or a non-recurring event). Returns `null` when the uid isn't
   * a known master — happens for transient state during a refresh.
   *
   * Callers use this to recover the real pubsub item id (so edit /
   * cancel target the master rather than a synthetic instance id)
   * and to read the master's RRULE / EXDATEs.
   */
  function findMaster(uid: string): CommunityEvent | null {
    for (const group of calendarItems.value) {
      if (group.master.uid === uid) return group.master;
    }
    return null;
  }

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
   * Replace an existing event in place. The pubsub item id is
   * preserved so sibling RSVPs (`<itemId>-rsvp-*`) stay grouped
   * under the same master after the next refresh.
   */
  async function edit(
    itemId: string,
    input: CommunityEventInput,
  ): Promise<CommunityEvent | null> {
    const client = xmppClient.value;
    const jid = options.communityJid.value;
    if (!client || !jid) return null;
    if (!input.summary.trim()) return null;
    isPosting.value = true;
    error.value = null;
    try {
      const event = await client.updateCommunityEvent(jid, itemId, input);
      events.value = events.value.map((e) => (e.id === itemId ? event : e));
      return event;
    } catch (err) {
      error.value = err instanceof Error ? err.message : String(err);
      return null;
    } finally {
      isPosting.value = false;
    }
  }

  /**
   * Cancel (retract) an event series entirely. Optimistically drops
   * the master from local state so the UI updates instantly; the
   * next refresh confirms.
   */
  async function cancel(itemId: string): Promise<boolean> {
    const client = xmppClient.value;
    const jid = options.communityJid.value;
    if (!client || !jid || !itemId) return false;
    try {
      await client.retractCommunityEvent(jid, itemId);
      events.value = events.value.filter((e) => e.id !== itemId);
      return true;
    } catch (err) {
      error.value = err instanceof Error ? err.message : String(err);
      return false;
    }
  }

  /**
   * Cancel a single occurrence of a recurring event by appending the
   * occurrence's DTSTART to the master's EXDATEs and re-publishing
   * the master in place. Falls back to a full series retract when
   * the master isn't recurring.
   */
  async function cancelInstance(
    masterUid: string,
    instanceDtstart: CalendarDateValue,
  ): Promise<boolean> {
    const master = findMaster(masterUid);
    if (!master) return false;
    if (!master.rrule) {
      return cancel(master.id);
    }
    const client = xmppClient.value;
    const jid = options.communityJid.value;
    if (!client || !jid) return false;
    const existing = new Map((master.exdates ?? []).map((value) => [calendarDateKey(value), value]));
    existing.set(calendarDateKey(instanceDtstart), instanceDtstart);
    const nextExdates = [...existing.values()].sort(compareCalendarDateValues);
    isPosting.value = true;
    error.value = null;
    try {
      const updated = await client.updateCommunityEvent(jid, master.id, {
        summary: master.summary,
        ...(master.description ? { description: master.description } : {}),
        ...(master.location ? { location: master.location } : {}),
        ...(master.organizer ? { organizer: master.organizer } : {}),
        ...(master.dtstart ? { dtstart: master.dtstart } : {}),
        ...(master.dtend ? { dtend: master.dtend } : {}),
        ...(master.rrule ? { rrule: master.rrule } : {}),
        exdates: nextExdates,
      });
      events.value = events.value.map((e) => (e.id === master.id ? updated : e));
      return true;
    } catch (err) {
      error.value = err instanceof Error ? err.message : String(err);
      return false;
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
    calendarItems,
    isLoading,
    isPosting,
    error,
    refresh,
    post,
    edit,
    cancel,
    cancelInstance,
    findMaster,
    rsvp,
    clear,
  };
}
