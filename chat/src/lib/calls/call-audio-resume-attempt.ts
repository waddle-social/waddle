export type CallAudioResumeAttempt = {
  matches(activeCallSid: string | null): boolean;
};

export class CallAudioResumeAttemptGuard {
  private currentAttemptId = 0;

  begin(callSid: string | null): CallAudioResumeAttempt {
    const attemptId = ++this.currentAttemptId;
    return {
      matches: (activeCallSid) =>
        attemptId === this.currentAttemptId && callSid === activeCallSid,
    };
  }

  reset(): void {
    this.currentAttemptId += 1;
  }
}
