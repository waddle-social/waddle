import { ref, type Ref } from "vue";
import type { ExtensionAnnotationAction, TimelineMessage } from "@/lib/chat-ui";
import { extensionActionStatusLabel } from "@/lib/chat-ui";
import { extensionCommandOutcome, type ExtensionCommandResult } from "@/lib/xmpp/extension-commands";

type ExtensionInvokeState = "loading" | "success" | "warning" | "error";
type ReadonlyRef<T> = Readonly<Ref<T>>;

export function useExtensionAnnotationActions(input: {
  annotations: ReadonlyRef<TimelineMessage["extensionAnnotations"]>;
  invokeExtensionAction: ReadonlyRef<((action: ExtensionAnnotationAction) => Promise<ExtensionCommandResult>) | undefined>;
}) {
  const states = ref<Record<string, { state: ExtensionInvokeState; detail?: string }>>({});

  function actionKey(annotationId: string, action: ExtensionAnnotationAction): string {
    return `${annotationId}:${action.launch?.id ?? action.route}:${action.label}`;
  }

  function actionState(annotationId: string, action: ExtensionAnnotationAction) {
    return states.value[actionKey(annotationId, action)];
  }

  function setActionState(
    annotationId: string,
    action: ExtensionAnnotationAction,
    state?: { state: ExtensionInvokeState; detail?: string },
  ) {
    const next = { ...states.value };
    const key = actionKey(annotationId, action);
    if (state) next[key] = state;
    else delete next[key];
    states.value = next;
  }

  async function invokeExtension(annotationId: string, action: ExtensionAnnotationAction) {
    if (!input.invokeExtensionAction.value || !action.launch) {
      setActionState(annotationId, action, { state: "error", detail: "This action cannot be invoked." });
      return;
    }
    setActionState(annotationId, action, { state: "loading" });
    try {
      const result = await input.invokeExtensionAction.value(action);
      setActionState(annotationId, action, extensionCommandOutcome(result));
    } catch (error) {
      setActionState(annotationId, action, {
        state: "error",
        detail: error instanceof Error ? error.message : "Action failed.",
      });
    }
  }

  function actionStatusLabel(annotationId: string, action: ExtensionAnnotationAction): string {
    return extensionActionStatusLabel(actionState(annotationId, action)?.state);
  }

  return {
    states,
    actionKey,
    actionState,
    actionStatusLabel,
    invokeExtension,
  };
}
