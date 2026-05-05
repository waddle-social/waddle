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
  specific?: string;
  text?: string;
}

export interface TunePublication {
  artist?: string;
  title?: string;
  source?: string;
  length?: number;
  rating?: number;
  track?: string;
  uri?: string;
}

export interface UserPepProfile {
  mood: MoodPublication | null;
  activity: ActivityPublication | null;
  tune: TunePublication | null;
}
