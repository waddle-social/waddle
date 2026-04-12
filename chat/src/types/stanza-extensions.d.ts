/**
 * TypeScript module augmentation for stanza protocol types.
 *
 * Extends the Message and Presence interfaces with fields defined by our
 * custom JXT definitions in xmpp-extensions.ts.
 */
declare module "stanza/protocol" {
  export interface Message {
    /** XEP-0372: Reference elements (mentions) */
    references?: WaddleReference[];
    /** XEP-0424: Message retraction */
    retract?: { id: string };
    /** XEP-0425: Message moderation via fastening */
    applyTo?: {
      id: string;
      moderated?: {
        retract?: boolean;
        reason?: string;
      };
    };
    /** XEP-0444: Message reactions */
    reactions?: {
      id: string;
      items: string[];
    };
    /** XEP-0446/0447: Stateless file sharing */
    fileSharing?: WaddleFileSharing;
    /** XEP-0449: Sticker */
    sticker?: { pack?: string };
    /** XEP-0482: Call invite (propose) */
    callPropose?: {
      id: string;
      audio?: boolean;
      video?: boolean;
      externalUri?: string;
    };
    /** XEP-0483: Online meeting */
    meeting?: {
      type?: string;
      url?: string;
      desc?: string;
    };
    /** XEP-0513: Explicit mentions container */
    explicitMentions?: {
      items: WaddleExplicitMention[];
    };
  }

  export interface Presence {
    /**
     * XEP-0317: Hats (overridden to use uri/title per current spec).
     * Note: stanza's built-in Hat type uses id/name which maps to the
     * outdated name/displayName XML attrs. Our JXT override maps
     * uri/title instead, so at runtime the objects have uri/title fields.
     */
    hats?: WaddleHat[];
  }
}

export interface WaddleReference {
  type: string;
  uri: string;
  begin?: string;
  end?: string;
}

export interface WaddleFileSharing {
  disposition?: string;
  name?: string;
  mediaType?: string;
  size?: string;
  width?: string;
  height?: string;
  desc?: string;
  url?: string;
}

export interface WaddleExplicitMention {
  type: string;
}

export interface WaddleHat {
  uri: string;
  title: string;
}
