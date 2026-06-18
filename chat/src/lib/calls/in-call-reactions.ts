import { atom } from "nanostores";

const IN_CALL_REACTION_TTL_MS = 2_400;
const MAX_IN_CALL_REACTIONS = 8;

export type InCallReactionAnimation = {
  id: string;
  sid: string;
  emoji: string;
  from: string;
  createdAt: number;
  expiresAt: number;
};

export type InCallReactionAction =
  | {
      kind: "received";
      sid: string;
      emoji: string;
      from: string;
      now: number;
    }
  | { kind: "expire"; now: number }
  | { kind: "clear" };

export const $inCallReactions = atom<InCallReactionAnimation[]>([]);

let sequence = 0;

export function reduceInCallReactions(
  current: readonly InCallReactionAnimation[],
  action: InCallReactionAction,
): InCallReactionAnimation[] {
  switch (action.kind) {
    case "received": {
      const active = current.filter((reaction) => reaction.expiresAt > action.now);
      const next: InCallReactionAnimation = {
        id: `${action.sid}:${action.now}:${sequence++}`,
        sid: action.sid,
        emoji: action.emoji,
        from: action.from,
        createdAt: action.now,
        expiresAt: action.now + IN_CALL_REACTION_TTL_MS,
      };
      return [...active, next].slice(-MAX_IN_CALL_REACTIONS);
    }
    case "expire":
      return current.filter((reaction) => reaction.expiresAt > action.now);
    case "clear":
      return [];
  }
}

export function receiveInCallReaction(input: {
  sid: string;
  emoji: string;
  from: string;
  now?: number;
}): void {
  const now = input.now ?? Date.now();
  $inCallReactions.set(
    reduceInCallReactions($inCallReactions.get(), {
      kind: "received",
      sid: input.sid,
      emoji: input.emoji,
      from: input.from,
      now,
    }),
  );
}

export function expireInCallReactions(now = Date.now()): void {
  $inCallReactions.set(
    reduceInCallReactions($inCallReactions.get(), {
      kind: "expire",
      now,
    }),
  );
}

export function clearInCallReactions(): void {
  $inCallReactions.set([]);
}
