import { mkdir } from "fs/promises";
import { join, dirname } from "path";
import { setup, createActor, assign, type SnapshotFrom } from "xstate";
import { getGitRoot } from "./git.js";
import type { State, Phase, Plan, BuildState, ReviewState, HistoryEntry } from "./types.js";

const STATE_DIR = ".ralph";
const STATE_FILE = "state.json";

function getStatePath(): string {
  return join(getGitRoot(), STATE_DIR, STATE_FILE);
}

type MachineContext = {
  iteration: number;
  plan: Plan | null;
  build: BuildState;
  review: ReviewState;
  history: HistoryEntry[];
};

export type MachineEvents =
  | { type: "START_BUILD"; reason: string }
  | { type: "SKIP_TO_REVIEW"; reason: string }
  | { type: "NOTHING_TO_DO"; reason: string }
  | { type: "IMPLEMENTATION_DONE"; reason: string }
  | { type: "BLOCKED"; reason: string }
  | { type: "CONTINUE_BUILDING"; reason: string }
  | { type: "APPROVED"; reason: string }
  | { type: "NEEDS_FIXES"; reason: string }
  | { type: "REJECTED"; reason: string }
  | { type: "NEXT_ITERATION"; reason: string }
  | { type: "MORE_REVIEW"; reason: string };

export const ralphMachine = setup({
  types: {
    context: {} as MachineContext,
    events: {} as MachineEvents,
  },
  actions: {
    recordTransition: assign({
      history: ({ context, event }, params: { from: Phase; to: Phase }) => [
        ...context.history,
        {
          phase: params.from,
          transition: params.to,
          reason: (event as MachineEvents).reason,
          timestamp: new Date().toISOString(),
        },
      ],
    }),
    incrementIteration: assign({
      iteration: ({ context }) => context.iteration + 1,
    }),
  },
}).createMachine({
  id: "ralph",
  initial: "PLAN",
  context: {
    iteration: 1,
    plan: null,
    build: { stepsCompleted: [], blockers: [] },
    review: { lastFeedback: null, issues: [] },
    history: [],
  },
  states: {
    PLAN: {
      on: {
        START_BUILD: {
          target: "BUILD",
          actions: [{ type: "recordTransition", params: { from: "PLAN", to: "BUILD" } }],
        },
        SKIP_TO_REVIEW: {
          target: "REVIEW",
          actions: [{ type: "recordTransition", params: { from: "PLAN", to: "REVIEW" } }],
        },
        NOTHING_TO_DO: {
          target: "END",
          actions: [{ type: "recordTransition", params: { from: "PLAN", to: "END" } }],
        },
      },
    },
    BUILD: {
      on: {
        IMPLEMENTATION_DONE: {
          target: "REVIEW",
          actions: [{ type: "recordTransition", params: { from: "BUILD", to: "REVIEW" } }],
        },
        BLOCKED: {
          target: "PLAN",
          actions: [{ type: "recordTransition", params: { from: "BUILD", to: "PLAN" } }],
        },
        CONTINUE_BUILDING: {
          target: "BUILD",
          actions: [{ type: "recordTransition", params: { from: "BUILD", to: "BUILD" } }],
        },
      },
    },
    REVIEW: {
      on: {
        APPROVED: {
          target: "END",
          actions: [{ type: "recordTransition", params: { from: "REVIEW", to: "END" } }],
        },
        NEEDS_FIXES: {
          target: "BUILD",
          actions: [{ type: "recordTransition", params: { from: "REVIEW", to: "BUILD" } }],
        },
        REJECTED: {
          target: "PLAN",
          actions: [{ type: "recordTransition", params: { from: "REVIEW", to: "PLAN" } }],
        },
        NEXT_ITERATION: {
          target: "PLAN",
          actions: [
            { type: "incrementIteration" },
            { type: "recordTransition", params: { from: "REVIEW", to: "PLAN" } },
          ],
        },
        MORE_REVIEW: {
          target: "REVIEW",
          actions: [{ type: "recordTransition", params: { from: "REVIEW", to: "REVIEW" } }],
        },
      },
    },
    END: {
      type: "final",
    },
  },
});

export type RalphSnapshot = SnapshotFrom<typeof ralphMachine>;

function getDefaultState(): State {
  return {
    iteration: 1,
    phase: "PLAN",
    timestamp: new Date().toISOString(),
    plan: null,
    build: { stepsCompleted: [], blockers: [] },
    review: { lastFeedback: null, issues: [] },
    history: [],
  };
}

export async function readState(): Promise<State> {
  const statePath = getStatePath();
  const file = Bun.file(statePath);

  if (!(await file.exists())) {
    return getDefaultState();
  }

  const content = await file.text();
  return JSON.parse(content) as State;
}

export async function writeState(state: State): Promise<State> {
  const statePath = getStatePath();
  const stateDir = dirname(statePath);

  await mkdir(stateDir, { recursive: true });

  const updated: State = {
    ...state,
    timestamp: new Date().toISOString(),
  };

  await Bun.write(statePath, JSON.stringify(updated, null, 2));
  return updated;
}

export function createRalphActor(state: State) {
  const snapshot: RalphSnapshot = {
    value: state.phase,
    context: {
      iteration: state.iteration,
      plan: state.plan,
      build: state.build,
      review: state.review,
      history: state.history,
    },
  } as RalphSnapshot;

  return createActor(ralphMachine, { snapshot });
}

export function snapshotToState(snapshot: RalphSnapshot): State {
  const phase = (typeof snapshot.value === "string" ? snapshot.value : Object.keys(snapshot.value)[0]) as Phase;
  return {
    iteration: snapshot.context.iteration,
    phase,
    timestamp: new Date().toISOString(),
    plan: snapshot.context.plan,
    build: snapshot.context.build,
    review: snapshot.context.review,
    history: snapshot.context.history,
  };
}

export async function transition(
  event: MachineEvents,
  stateUpdates: Partial<MachineContext> = {}
): Promise<State> {
  const currentState = await readState();
  const actor = createRalphActor(currentState);

  actor.start();

  if (Object.keys(stateUpdates).length > 0) {
    Object.assign(actor.getSnapshot().context, stateUpdates);
  }

  actor.send(event);

  const newSnapshot = actor.getSnapshot();
  actor.stop();

  if (newSnapshot.value === currentState.phase && event.type !== "CONTINUE_BUILDING" && event.type !== "MORE_REVIEW") {
    throw new Error(`Invalid transition: event ${event.type} not allowed from ${currentState.phase}`);
  }

  const newState = snapshotToState(newSnapshot);
  return writeState(newState);
}

export async function resetState(): Promise<State> {
  return writeState(getDefaultState());
}

const VALID_EVENTS: Record<Phase, MachineEvents["type"][]> = {
  PLAN: ["START_BUILD", "SKIP_TO_REVIEW", "NOTHING_TO_DO"],
  BUILD: ["IMPLEMENTATION_DONE", "BLOCKED", "CONTINUE_BUILDING"],
  REVIEW: ["APPROVED", "NEEDS_FIXES", "REJECTED", "NEXT_ITERATION", "MORE_REVIEW"],
  END: [],
};

export function canTransition(phase: Phase, eventType: MachineEvents["type"]): boolean {
  return VALID_EVENTS[phase].includes(eventType);
}
