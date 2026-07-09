import { context, SpanStatusCode, trace, type Span } from "@opentelemetry/api";
import { categorizedOperationErrorForTelemetry } from "./error-classification";
import { getFaro, observeTelemetry } from "./runtime";

const TRACER_NAME = "waddle-chat";

/**
 * Run `fn` inside a manually-started OpenTelemetry span. Use this for
 * operations that aren't `fetch` / `XHR` (so Faro's automatic span
 * wrapping doesn't cover them) — XMPP connect, message send, room
 * join. When Faro isn't initialized, `fn` runs with no span overhead.
 *
 * The span is also made the *active context* for the duration of
 * `fn`, so any nested auto-instrumented work (a `fetch` called inside
 * the callback, a further `withSpan`) attaches to this span instead
 * of whatever root context was active outside. The backend therefore
 * sees the manual span as the parent of the cross-origin HTTP span,
 * not an orphaned sibling.
 *
 * The callback deliberately receives no SDK object. Exposing a raw span would
 * let a faulty `setAttribute` implementation turn telemetry into a product
 * failure; all span interaction stays behind the no-throw boundary here.
 */
export async function withSpan<T>(
  name: string,
  attributes: Record<string, string | number | boolean>,
  fn: () => Promise<T>,
): Promise<T> {
  if (!getFaro()) return fn();

  let span: Span;
  try {
    span = trace.getTracer(TRACER_NAME).startSpan(name, { attributes });
  } catch {
    return fn();
  }

  let activeContext: ReturnType<typeof context.active>;
  try {
    activeContext = trace.setSpan(context.active(), span);
  } catch {
    observeTelemetry(() => span.end());
    return fn();
  }

  type CoreOutcome =
    | { ok: true; value: T }
    | { ok: false; error: unknown };
  let execution: Promise<CoreOutcome> | null = null;
  const executeCore = async (): Promise<CoreOutcome> => {
    try {
      const result = await fn();
      observeTelemetry(() => span.setStatus({ code: SpanStatusCode.OK }));
      return { ok: true, value: result };
    } catch (err) {
      observeTelemetry(() => {
        const categorizedError = categorizedOperationErrorForTelemetry();
        span.setStatus({
          code: SpanStatusCode.ERROR,
          message: categorizedError.message,
        });
        span.recordException(categorizedError);
      });
      return { ok: false, error: err };
    } finally {
      observeTelemetry(() => span.end());
    }
  };

  const startCore = (): Promise<CoreOutcome> => {
    if (execution) return execution;

    // Publish the authoritative promise before invoking product code. A
    // context manager may retain and re-enter its callback, including while
    // `fn` is starting, but every invocation must observe this same promise.
    let settle!: (outcome: CoreOutcome) => void;
    const retainedExecution = new Promise<CoreOutcome>((resolve) => {
      settle = resolve;
    });
    execution = retainedExecution;
    void executeCore().then(settle, (error: unknown) => {
      settle({ ok: false, error });
    });
    return retainedExecution;
  };

  // Context-manager return values are telemetry-owned and untrusted. It may
  // throw, return an unrelated never-settling thenable, skip the callback, or
  // retain it for a later invocation. Launch inside the context when offered,
  // discard the return synchronously, then start directly only if necessary.
  try {
    void context.with(activeContext, startCore);
  } catch {
    // The retained execution, when present, remains authoritative.
  }
  const outcome = await startCore();
  if (!outcome.ok) throw outcome.error;
  return outcome.value;
}
