import type { AudioProcessorOptions, Track, TrackProcessor } from "livekit-client";

/**
 * A LiveKit audio `TrackProcessor` we attach to the local mic via
 * `localAudioTrack.setProcessor`. We implement the interface directly against
 * livekit-client (no `@livekit/track-processors` — it has no audio pipeline).
 */
export type AudioNoiseProcessor = TrackProcessor<Track.Kind.Audio, AudioProcessorOptions>;

/**
 * Shared Web Audio plumbing for a model that exposes its denoiser as a single
 * `AudioWorkletNode`: build `source → worklet → MediaStreamDestination` on the
 * room's shared `AudioContext` and publish the destination track as
 * `processedTrack`. LiveKit calls `restart` on a track swap (device change /
 * constraint restart) and `destroy` on stop, so the graph is torn down and
 * rebuilt cleanly. Subclasses supply only how to make (and dispose) the node.
 *
 * This is the thin Web-Audio boundary — verified manually, not in unit tests
 * (there is no `AudioContext`/`AudioWorklet` in the test runtime).
 */
export abstract class WorkletNoiseProcessor implements AudioNoiseProcessor {
  abstract readonly name: string;
  processedTrack?: MediaStreamTrack;

  private sourceNode?: MediaStreamAudioSourceNode;
  private workletNode?: AudioWorkletNode;
  private destinationNode?: MediaStreamAudioDestinationNode;

  /** Load the worklet module and construct the model's node. */
  protected abstract createWorkletNode(context: AudioContext): Promise<AudioWorkletNode>;

  /** Optional model-specific node teardown (e.g. `node.destroy()`). */
  protected disposeWorkletNode(_node: AudioWorkletNode): void {}

  async init(opts: AudioProcessorOptions): Promise<void> {
    const context = opts.audioContext;
    const source = context.createMediaStreamSource(new MediaStream([opts.track]));
    const node = await this.createWorkletNode(context);
    const destination = context.createMediaStreamDestination();
    source.connect(node);
    node.connect(destination);
    this.sourceNode = source;
    this.workletNode = node;
    this.destinationNode = destination;
    this.processedTrack = destination.stream.getAudioTracks()[0];
  }

  async restart(opts: AudioProcessorOptions): Promise<void> {
    await this.destroy();
    await this.init(opts);
  }

  async destroy(): Promise<void> {
    this.sourceNode?.disconnect();
    if (this.workletNode) {
      this.workletNode.disconnect();
      this.disposeWorkletNode(this.workletNode);
    }
    this.destinationNode?.disconnect();
    // A `MediaStreamAudioDestinationNode`'s output track does NOT end when the
    // graph is torn down — only an explicit stop() transitions it to "ended".
    // Without this, rapid device switches leak "live" ghost tracks until GC.
    this.processedTrack?.stop();
    this.sourceNode = undefined;
    this.workletNode = undefined;
    this.destinationNode = undefined;
    this.processedTrack = undefined;
  }
}
