export type DisposableXmppClient = {
  dispose: () => Promise<void>;
};

/** Keeps asynchronous auth bootstrap single-owner. */
export function createXmppBootstrapCoordinator<TClient extends DisposableXmppClient>(
  readClient: () => TClient | null,
  writeClient: (client: TClient | null) => void,
) {
  let generation = 0;
  const release = (client: TClient | null): void => {
    void client?.dispose().catch(() => undefined);
  };
  return {
    begin(): number {
      generation += 1;
      return generation;
    },
    isCurrent(candidate: number): boolean {
      return candidate === generation;
    },
    invalidate(): void {
      generation += 1;
    },
    detach(): void {
      generation += 1;
      const incumbent = readClient();
      writeClient(null);
      release(incumbent);
    },
    detachIfCurrent(candidate: number): boolean {
      if (candidate !== generation) return false;
      this.detach();
      return true;
    },
    async replace(candidate: number, create: () => TClient): Promise<void> {
      const incumbent = readClient();
      writeClient(null);
      // `dispose()` releases logical ownership synchronously, but physical
      // call/room/socket teardown can stall. Do not let that stall block a
      // fresh authenticated owner from becoming available.
      release(incumbent);
      if (candidate !== generation) return;
      const successor = create();
      if (candidate !== generation) {
        release(successor);
        return;
      }
      writeClient(successor);
    },
  };
}
