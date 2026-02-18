export type Phase = "PLAN" | "BUILD" | "REVIEW" | "END";

export interface Config {
  maxRetries: number;
  maxTurns: number;
  targetDoc: string;
  dryRun: boolean;
  startPhase?: Phase;
}

export interface Plan {
  task: string;
  files: string[];
  steps: string[];
  acceptanceCriteria: string[];
}

export interface BuildState {
  stepsCompleted: string[];
  blockers: string[];
}

export interface ReviewState {
  lastFeedback: string | null;
  issues: string[];
}

export interface HistoryEntry {
  phase: Phase;
  transition: Phase;
  reason: string;
  timestamp: string;
}

export interface State {
  iteration: number;
  phase: Phase;
  timestamp: string;
  plan: Plan | null;
  build: BuildState;
  review: ReviewState;
  history: HistoryEntry[];
}

export interface PhaseResult {
  nextPhase: Phase;
  reason: string;
  stateUpdates: Partial<State>;
}
