export function sameThreadStack(left: readonly string[], right: readonly string[]): boolean {
  return left.length === right.length && left.every((threadId, index) => threadId === right[index]);
}

export function nextThreadStack(baseStack: readonly string[], threadId: string): string[] {
  return baseStack[baseStack.length - 1] === threadId ? [...baseStack] : [...baseStack, threadId];
}
