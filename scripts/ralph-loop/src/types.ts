export interface Config {
  iterations: number;
  targetDoc: string;
  dryRun: boolean;
}

export interface IterationResult {
  iteration: number;
  success: boolean;
  commitSummary: string | null;
  error?: string;
  duration: number;
}

export interface IterationLog {
  iteration: number;
  timestamp: string;
  config: Config;
  diff: string;
  result: IterationResult;
}
