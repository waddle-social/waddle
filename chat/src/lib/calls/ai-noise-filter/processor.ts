import {
  TrackEvent,
  type AudioProcessorOptions,
  type LocalTrack,
  type Track,
  type TrackProcessor,
} from "livekit-client";

/**
 * A LiveKit audio `TrackProcessor` we attach to the local mic via
 * `localAudioTrack.setProcessor`. We implement this denoise pipeline directly
 * because each model is exposed as its own AudioWorklet graph.
 */
export type AudioNoiseProcessor = TrackProcessor<Track.Kind.Audio, AudioProcessorOptions>;

/**
 * One built denoise graph: `source → worklet → MediaStreamDestination`, plus the
 * private capture-track clone that feeds it and the mute listener bound to the
 * owning track. Held as a unit so a graph can be torn down independently of the
 * one currently published (see `restart`).
 */
type DenoiseGraph = {
  clone: MediaStreamTrack;
  source: MediaStreamAudioSourceNode;
  worklet: AudioWorkletNode;
  destination: MediaStreamAudioDestinationNode;
  processedTrack?: MediaStreamTrack;
  muteSource?: LocalTrack;
  mirror?: () => void;
};

/**
 * Shared Web Audio plumbing for a model that exposes its denoiser as a single
 * `AudioWorkletNode`: build `source → worklet → MediaStreamDestination` on the
 * room's shared `AudioContext` and publish the destination track as
 * `processedTrack`. LiveKit calls `restart` on a track swap (device change /
 * constraint restart) and `destroy` on stop. Subclasses supply only how to make
 * (and dispose) the node.
 *
 * The source is built from a CLONE of `opts.track`, never the track itself.
 * When a model attaches, the engine restarts mic capture to drop the now
 * redundant browser noise suppression, and LiveKit's restart STOPS the
 * underlying capture track (its `readyState` flips to "ended"). A
 * `MediaStreamAudioSourceNode` reading a stopped track emits silence — which
 * is exactly the "filter on → no audio sent" bug. A clone has an independent
 * lifecycle tied only to the device, so our graph keeps pulling audio across
 * LiveKit recycling its own track; we stop the clone ourselves on teardown.
 * A device switch hands us a fresh `opts.track` via `restart`, so the new
 * device's track is the one cloned here.
 *
 * `restart` builds the new graph BEFORE tearing down the old one: if the build
 * fails (e.g. a transient worklet/wasm load), the existing graph keeps running
 * — its clone is independent of the capture track LiveKit just recycled — so we
 * never strand the published track on a dead graph (silence) nor leave a live
 * processor with no output (a lying "filter on" the reconciler can't recover).
 *
 * Because the clone's `enabled` is independent of LiveKit's capture track, it
 * would NOT follow a mute: with `stopMicTrackOnMute: false`, muting only sets
 * `enabled = false` on LiveKit's track, leaving the clone live and the user
 * audible while "muted". So we mirror the owning `LocalTrack`'s mute state onto
 * the clone (`opts.localTrack` emits `Muted`/`Unmuted`).
 *
 * This is the thin Web-Audio boundary — verified manually, not in unit tests
 * (there is no `AudioContext`/`AudioWorklet` in the test runtime).
 */
export abstract class WorkletNoiseProcessor implements AudioNoiseProcessor {
  abstract readonly name: string;
  processedTrack?: MediaStreamTrack;

  private graph?: DenoiseGraph;

  /** Load the worklet module and construct the model's node. */
  protected abstract createWorkletNode(context: AudioContext): Promise<AudioWorkletNode>;

  /** Optional model-specific node teardown (e.g. `node.destroy()`). */
  protected disposeWorkletNode(_node: AudioWorkletNode): void {}

  async init(opts: AudioProcessorOptions): Promise<void> {
    const graph = await this.buildGraph(opts);
    this.graph = graph;
    this.processedTrack = graph.processedTrack;
  }

  async restart(opts: AudioProcessorOptions): Promise<void> {
    let next: DenoiseGraph;
    try {
      next = await this.buildGraph(opts);
    } catch {
      // Building the replacement failed. Keep the existing graph published
      // rather than tearing it down for nothing: its clone is independent of
      // the capture track LiveKit just recycled, so it keeps producing audio,
      // and leaving `processedTrack` unchanged means LiveKit keeps publishing
      // the working filtered track instead of falling to silence.
      return;
    }
    const previous = this.graph;
    this.graph = next;
    this.processedTrack = next.processedTrack;
    if (previous) this.teardownGraph(previous);
  }

  async destroy(): Promise<void> {
    if (this.graph) this.teardownGraph(this.graph);
    this.graph = undefined;
    this.processedTrack = undefined;
  }

  /** Build (but do not install) a fresh denoise graph from `opts`. */
  private async buildGraph(opts: AudioProcessorOptions): Promise<DenoiseGraph> {
    const context = opts.audioContext;
    // Clone so our source outlives LiveKit stopping its own capture track on a
    // restart (see class doc) — this is what stops the filter publishing silence.
    const clone = opts.track.clone();
    try {
      const source = context.createMediaStreamSource(new MediaStream([clone]));
      const worklet = await this.createWorkletNode(context);
      const destination = context.createMediaStreamDestination();
      source.connect(worklet);
      worklet.connect(destination);
      const graph: DenoiseGraph = {
        clone,
        source,
        worklet,
        destination,
        processedTrack: destination.stream.getAudioTracks()[0],
      };
      this.mirrorMute(opts.localTrack, graph);
      return graph;
    } catch (err) {
      // Failed partway (e.g. the worklet module / wasm failed to load). Stop the
      // clone so it doesn't hold the input device open, then propagate.
      clone.stop();
      throw err;
    }
  }

  /** Disconnect, dispose, and release every resource owned by `graph`. */
  private teardownGraph(graph: DenoiseGraph): void {
    if (graph.muteSource && graph.mirror) {
      graph.muteSource.off(TrackEvent.Muted, graph.mirror);
      graph.muteSource.off(TrackEvent.Unmuted, graph.mirror);
    }
    graph.source.disconnect();
    graph.worklet.disconnect();
    this.disposeWorkletNode(graph.worklet);
    graph.destination.disconnect();
    // A `MediaStreamAudioDestinationNode`'s output track does NOT end when the
    // graph is torn down — only an explicit stop() transitions it to "ended".
    // Without this, rapid device switches leak "live" ghost tracks until GC.
    graph.processedTrack?.stop();
    // Release our private clone so it stops holding the input device open.
    graph.clone.stop();
  }

  /**
   * Keep the graph's clone `enabled` in lockstep with the owning track's mute
   * state, so muting the mic actually silences the published processed track.
   * No-op if LiveKit didn't supply the owning track (it always does in practice).
   */
  private mirrorMute(localTrack: LocalTrack | undefined, graph: DenoiseGraph): void {
    if (!localTrack) return;
    const mirror = (): void => {
      // Disabling the clone makes the source feed digital silence into the
      // worklet, so the published processed track goes silent — that is what
      // makes muting actually mute while a filter is attached.
      graph.clone.enabled = !localTrack.isMuted;
    };
    mirror();
    localTrack.on(TrackEvent.Muted, mirror);
    localTrack.on(TrackEvent.Unmuted, mirror);
    graph.muteSource = localTrack;
    graph.mirror = mirror;
  }
}
