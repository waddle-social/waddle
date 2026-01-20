export function buildIterationPrompt(
  n: number,
  diff: string,
  targetDoc: string
): string {
  return `
# Ralph Loop - Iteration ${n}

You are working on the project plan document: ${targetDoc}

## Previous Changes
${diff ? `\`\`\`diff\n${diff}\n\`\`\`` : "This is the first iteration."}

## Your Task (Full Cycle)

### 1. PLAN
- Read ${targetDoc} and understand current state
- Review the diff above (if any) - what changed in the last iteration?
- Identify the next highest-priority incomplete task

### 2. IMPLEMENT
- Make progress on the identified task
- Update ${targetDoc} to reflect progress
- Create/modify any necessary code or documentation

### 3. REVIEW
- Verify your changes are correct and complete
- Check for any issues or inconsistencies

### 4. REFINE
- Make any necessary adjustments
- Ensure ${targetDoc} accurately reflects the new state
- Summarize what you accomplished for the commit message

Output your commit summary at the end in this format:
<commit-summary>
Brief description of changes made
</commit-summary>
`.trim();
}
