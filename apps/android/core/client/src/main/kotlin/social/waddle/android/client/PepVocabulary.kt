package social.waddle.android.client

/**
 * Closed PEP status vocabularies, mirroring the web client's
 * `pep-types.ts` (and the server's typed XEP-0107/0108 enums). The FFI
 * rejects kinds outside these sets with `InvalidArgument`, so pickers
 * must offer exactly these values.
 */

/** XEP-0107 §3: the 84 defined mood element names. */
val MOOD_KINDS: List<String> = listOf(
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
)

/** XEP-0108 §3.1: the 12 defined general activity categories. */
val GENERAL_ACTIVITIES: List<String> = listOf(
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
)

/** XEP-0108 §3.2 `talking` specifics that mean "on a call" (web
 *  `pep-types.ts` parity; ADR-010's in-call presence overlay rides
 *  exactly these). */
const val ACTIVITY_GENERAL_TALKING: String = "talking"
const val ACTIVITY_SPECIFIC_ON_THE_PHONE: String = "on_the_phone"
const val ACTIVITY_SPECIFIC_ON_VIDEO_PHONE: String = "on_video_phone"
