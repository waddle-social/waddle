import { ProviderLifecycle } from "./provider-lifecycle";

export type DisposableProviderClient = {
  dispose(): Promise<void>;
};

export type StatusAwareProviderClient<Status> = DisposableProviderClient & {
  setStatusHandler(handler: (status: Status) => void): void;
};

export type ProviderClientCoordinatorDependencies<
  Session,
  Client extends DisposableProviderClient,
> = {
  getClient(): Client | null;
  setClient(client: Client | null): void;
  createClient(session: Session): Client;
  configureClient(client: Client): void;
  disposeClient(client: Client): Promise<void>;
};

export type InstrumentedProviderClientCoordinatorDependencies<
  Session,
  Status,
  Client extends StatusAwareProviderClient<Status>,
> = Omit<
  ProviderClientCoordinatorDependencies<Session, Client>,
  "configureClient"
> & {
  instrumentClient(client: Client): void;
  handleStatus(status: Status): void;
};

/**
 * Owns provider bootstrap, client replacement, and terminal disposal under one
 * monotonic lifecycle epoch. No candidate can install after disposal begins,
 * and every predecessor or failed candidate is disposed before the serialized
 * operation settles.
 */
export class ProviderClientCoordinator<
  Session,
  Client extends DisposableProviderClient,
> {
  private readonly lifecycle = new ProviderLifecycle();

  constructor(
    private readonly dependencies: ProviderClientCoordinatorDependencies<
      Session,
      Client
    >,
  ) {}

  get state() {
    return this.lifecycle.state;
  }

  captureActiveEpoch(): number {
    return this.lifecycle.captureActiveEpoch();
  }

  assertActive(expectedEpoch: number): void {
    this.lifecycle.assertActive(expectedEpoch);
  }

  async bootstrap(
    loadSession: () => Promise<Session | null>,
    afterLoad: () => void,
  ): Promise<void> {
    const expectedEpoch = this.captureActiveEpoch();
    const session = await loadSession();
    this.assertActive(expectedEpoch);
    afterLoad();
    this.assertActive(expectedEpoch);
    await this.replace(session, expectedEpoch);
  }

  replace(
    nextSession: Session | null,
    expectedEpoch = this.captureActiveEpoch(),
  ): Promise<void> {
    return this.lifecycle.serialize(expectedEpoch, async (assertCurrent) => {
      const predecessor = this.dependencies.getClient();
      this.dependencies.setClient(null);
      if (predecessor) {
        await this.dependencies.disposeClient(predecessor);
      }
      assertCurrent();
      if (!nextSession) return;

      let candidate: Client | null = this.dependencies.createClient(nextSession);
      try {
        this.dependencies.configureClient(candidate);
        assertCurrent();
        this.dependencies.setClient(candidate);
        candidate = null;
      } catch (setupError) {
        if (!candidate) throw setupError;
        try {
          await this.dependencies.disposeClient(candidate);
        } catch (disposeError) {
          throw new AggregateError(
            [setupError, disposeError],
            "XMPP provider candidate setup and disposal failed",
          );
        }
        throw setupError;
      }
    });
  }

  dispose(): Promise<void> {
    return this.lifecycle.dispose(async () => {
      const predecessor = this.dependencies.getClient();
      this.dependencies.setClient(null);
      if (predecessor) {
        await this.dependencies.disposeClient(predecessor);
      }
    });
  }
}

/**
 * Builds the provider coordinator used by the Vue integration boundary. Client
 * instrumentation and status routing are installed before a candidate can
 * become visible through the shared connection store.
 */
export function createInstrumentedProviderClientCoordinator<
  Session,
  Status,
  Client extends StatusAwareProviderClient<Status>,
>(
  dependencies: InstrumentedProviderClientCoordinatorDependencies<
    Session,
    Status,
    Client
  >,
): ProviderClientCoordinator<Session, Client> {
  return new ProviderClientCoordinator({
    getClient: dependencies.getClient,
    setClient: dependencies.setClient,
    createClient: dependencies.createClient,
    configureClient: (client) => {
      dependencies.instrumentClient(client);
      client.setStatusHandler(dependencies.handleStatus);
    },
    disposeClient: dependencies.disposeClient,
  });
}
