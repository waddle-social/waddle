import { z } from "zod";

export const PlanOutputSchema = z.object({
  transition: z.enum(["BUILD", "END", "REVIEW"]),
  reason: z.string().describe("Why this transition was chosen"),
  plan: z
    .object({
      task: z.string().describe("The task to implement"),
      files: z.array(z.string()).describe("Files that will be modified"),
      steps: z.array(z.string()).describe("Implementation steps"),
      acceptanceCriteria: z.array(z.string()).describe("Criteria for completion"),
    })
    .nullable()
    .describe("The implementation plan, null if transitioning to END"),
});

export type PlanOutput = z.infer<typeof PlanOutputSchema>;

export const BuildOutputSchema = z.object({
  transition: z.enum(["REVIEW", "PLAN", "BUILD"]),
  reason: z.string().describe("Why this transition was chosen"),
  stepsCompleted: z.array(z.string()).describe("Steps that were completed"),
  blockers: z.array(z.string()).describe("Any blockers encountered"),
});

export type BuildOutput = z.infer<typeof BuildOutputSchema>;

export const ReviewOutputSchema = z.object({
  transition: z.enum(["PLAN", "BUILD", "END", "REVIEW"]),
  reason: z.string().describe("Why this transition was chosen"),
  feedback: z.string().nullable().describe("Review feedback"),
  issues: z.array(z.string()).describe("Issues found during review"),
  approved: z.boolean().describe("Whether the implementation is approved"),
});

export type ReviewOutput = z.infer<typeof ReviewOutputSchema>;

export function transitionEventFromPlanOutput(
  output: PlanOutput
): { type: "START_BUILD" | "SKIP_TO_REVIEW" | "NOTHING_TO_DO"; reason: string } {
  switch (output.transition) {
    case "BUILD":
      return { type: "START_BUILD", reason: output.reason };
    case "REVIEW":
      return { type: "SKIP_TO_REVIEW", reason: output.reason };
    case "END":
      return { type: "NOTHING_TO_DO", reason: output.reason };
  }
}

export function transitionEventFromBuildOutput(
  output: BuildOutput
): { type: "IMPLEMENTATION_DONE" | "BLOCKED" | "CONTINUE_BUILDING"; reason: string } {
  switch (output.transition) {
    case "REVIEW":
      return { type: "IMPLEMENTATION_DONE", reason: output.reason };
    case "PLAN":
      return { type: "BLOCKED", reason: output.reason };
    case "BUILD":
      return { type: "CONTINUE_BUILDING", reason: output.reason };
  }
}

export function transitionEventFromReviewOutput(
  output: ReviewOutput
): { type: "APPROVED" | "NEEDS_FIXES" | "REJECTED" | "NEXT_ITERATION" | "MORE_REVIEW"; reason: string } {
  switch (output.transition) {
    case "END":
      return { type: "APPROVED", reason: output.reason };
    case "BUILD":
      return { type: "NEEDS_FIXES", reason: output.reason };
    case "PLAN":
      return output.approved
        ? { type: "NEXT_ITERATION", reason: output.reason }
        : { type: "REJECTED", reason: output.reason };
    case "REVIEW":
      return { type: "MORE_REVIEW", reason: output.reason };
  }
}
