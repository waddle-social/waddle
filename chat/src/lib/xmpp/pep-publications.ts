/**
 * XEP-0107 Mood, XEP-0108 Activity, XEP-0118 Tune.
 *
 * Thin typed wrappers around stanza.js's PEP helpers
 * (`xmpp.publishMood`, `xmpp.publishActivity`, `xmpp.publishTune`) that
 * narrow the payload to the closed enums Waddle's server understands and
 * provide explicit retraction helpers.
 *
 * Wire format alignment:
 *   server/crates/waddle-xmpp/src/xep/xep0107.rs  (mood)
 *   server/crates/waddle-xmpp/src/xep/xep0108.rs  (activity)
 *   server/crates/waddle-xmpp/src/xep/xep0118.rs  (tune)
 */
import type { Agent } from "stanza";

// -- Namespaces -----------------------------------------------------------

const NS_MOOD = "http://jabber.org/protocol/mood";
const NS_TUNE = "http://jabber.org/protocol/tune";

// -- Mood (XEP-0107) ----------------------------------------------------

/** Closed set of mood keywords from XEP-0107 §3 (matches stanza.js USER_MOODS). */
export const MOOD_KINDS = [
  "afraid", "amazed", "amorous", "angry", "annoyed", "anxious",
  "aroused", "ashamed", "bored", "brave", "calm", "cautious",
  "cold", "confident", "confused", "contemplative", "contented",
  "cranky", "crazy", "creative", "curious", "dejected", "depressed",
  "disappointed", "disgusted", "dismayed", "distracted", "embarrassed",
  "envious", "excited", "flirtatious", "frustrated", "grateful",
  "grieving", "grumpy", "guilty", "happy", "hopeful", "hot",
  "humbled", "humiliated", "hungry", "hurt", "impressed", "in_awe",
  "in_love", "indignant", "interested", "intoxicated", "invincible",
  "jealous", "lonely", "lost", "lucky", "mean", "moody", "nervous",
  "neutral", "offended", "outraged", "playful", "proud", "relaxed",
  "relieved", "remorseful", "restless", "sad", "sarcastic",
  "satisfied", "serious", "shocked", "shy", "sick", "sleepy",
  "spontaneous", "stressed", "strong", "surprised", "thankful",
  "thirsty", "tired", "undefined", "weak", "worried",
] as const;

export type MoodKind = typeof MOOD_KINDS[number];

export interface MoodPublication {
  kind: MoodKind;
  text?: string;
}

export async function publishMood(xmpp: Agent, mood: MoodPublication): Promise<void> {
  await xmpp.publishMood({ value: mood.kind, text: mood.text });
}

/** Publish an empty mood payload — clears the user's current mood. */
export async function retractMood(xmpp: Agent): Promise<void> {
  await xmpp.publishMood({});
}

// -- Activity (XEP-0108) ------------------------------------------------

const NS_ACTIVITY = "http://jabber.org/protocol/activity";

/** General activity categories from XEP-0108 §3.1 (matches USER_ACTIVITY_GENERAL). */
export const GENERAL_ACTIVITIES = [
  "doing_chores",
  "drinking",
  "eating",
  "exercising",
  "grooming",
  "having_appointment",
  "inactive",
  "relaxing",
  "talking",
  "traveling",
  "working",
  "undefined",
] as const;

export type GeneralActivity = typeof GENERAL_ACTIVITIES[number];

export interface ActivityPublication {
  general: GeneralActivity;
  /** Optional well-known sub-activity name (snake_case). */
  specific?: string;
  text?: string;
}

export async function publishActivity(xmpp: Agent, activity: ActivityPublication): Promise<void> {
  const tuple: [string] | [string, string] = activity.specific
    ? [activity.general, activity.specific]
    : [activity.general];
  await xmpp.publishActivity({ activity: tuple, text: activity.text });
}

export async function retractActivity(xmpp: Agent): Promise<void> {
  await xmpp.publish("", NS_ACTIVITY, { itemType: NS_ACTIVITY });
}

// -- Tune (XEP-0118) ----------------------------------------------------

export interface TunePublication {
  artist?: string;
  title?: string;
  source?: string;
  /** Track length in seconds. */
  length?: number;
  /** User rating 1-10. */
  rating?: number;
  track?: string;
  uri?: string;
}

export async function publishTune(xmpp: Agent, tune: TunePublication): Promise<void> {
  if (tune.rating !== undefined && (tune.rating < 1 || tune.rating > 10)) {
    throw new Error(`Tune rating must be between 1 and 10, got ${tune.rating}`);
  }
  await xmpp.publishTune({
    artist: tune.artist,
    title: tune.title,
    source: tune.source,
    length: tune.length,
    rating: tune.rating,
    track: tune.track,
    uri: tune.uri,
  });
}

export async function retractTune(xmpp: Agent): Promise<void> {
  await xmpp.publishTune({});
}

// -- Fetch PEP items from other users -----------------------------------

export interface UserPepProfile {
  mood: MoodPublication | null;
  activity: ActivityPublication | null;
  tune: TunePublication | null;
}

function isSilentPepError(err: unknown): boolean {
  if (!(err instanceof Error)) return false;
  const msg = err.message.toLowerCase();
  return (
    msg.includes("item-not-found") ||
    msg.includes("feature-not-implemented") ||
    msg.includes("service-unavailable") ||
    msg.includes("forbidden") ||
    msg.includes("not-allowed")
  );
}

async function fetchMood(xmpp: Agent, jid: string): Promise<MoodPublication | null> {
  try {
    const result = await xmpp.getItems(jid, NS_MOOD);
    const item = result?.items?.[0];
    const content = item?.content as { value?: string; text?: string } | undefined;
    if (!content?.value) return null;
    const kind = content.value as MoodKind;
    if (!MOOD_KINDS.includes(kind)) return null;
    return { kind, ...(content.text ? { text: content.text } : {}) };
  } catch (err) {
    if (!isSilentPepError(err)) console.warn("[pep] failed to fetch mood from", jid, err);
    return null;
  }
}

async function fetchActivity(xmpp: Agent, jid: string): Promise<ActivityPublication | null> {
  try {
    const result = await xmpp.getItems(jid, NS_ACTIVITY);
    const item = result?.items?.[0];
    const content = item?.content as { activity?: string | [string] | [string, string]; text?: string } | undefined;
    if (!content?.activity) return null;
    const tuple = Array.isArray(content.activity) ? content.activity : [content.activity];
    const general = tuple[0] as GeneralActivity;
    if (!GENERAL_ACTIVITIES.includes(general)) return null;
    return {
      general,
      ...(tuple[1] ? { specific: tuple[1] } : {}),
      ...(content.text ? { text: content.text } : {}),
    };
  } catch (err) {
    if (!isSilentPepError(err)) console.warn("[pep] failed to fetch activity from", jid, err);
    return null;
  }
}

async function fetchTune(xmpp: Agent, jid: string): Promise<TunePublication | null> {
  try {
    const result = await xmpp.getItems(jid, NS_TUNE);
    const item = result?.items?.[0];
    const content = item?.content as TunePublication | undefined;
    if (!content) return null;
    const hasAnyField = content.artist || content.title || content.source ||
      content.length || content.rating || content.track || content.uri;
    if (!hasAnyField) return null;
    return {
      ...(content.artist ? { artist: content.artist } : {}),
      ...(content.title ? { title: content.title } : {}),
      ...(content.source ? { source: content.source } : {}),
      ...(content.length ? { length: content.length } : {}),
      ...(content.rating ? { rating: content.rating } : {}),
      ...(content.track ? { track: content.track } : {}),
      ...(content.uri ? { uri: content.uri } : {}),
    };
  } catch (err) {
    if (!isSilentPepError(err)) console.warn("[pep] failed to fetch tune from", jid, err);
    return null;
  }
}

async function fetchUserPepProfile(xmpp: Agent, jid: string): Promise<UserPepProfile> {
  const [moodResult, activityResult, tuneResult] = await Promise.allSettled([
    fetchMood(xmpp, jid),
    fetchActivity(xmpp, jid),
    fetchTune(xmpp, jid),
  ]);
  return {
    mood: moodResult.status === "fulfilled" ? moodResult.value : null,
    activity: activityResult.status === "fulfilled" ? activityResult.value : null,
    tune: tuneResult.status === "fulfilled" ? tuneResult.value : null,
  };
}
