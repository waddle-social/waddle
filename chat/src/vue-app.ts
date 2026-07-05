import type { App, ComponentPublicInstance } from "vue";
import { QueryClient, VueQueryPlugin } from "@tanstack/vue-query";
import { reportVueError } from "@/lib/telemetry";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 30_000,
      refetchOnWindowFocus: false,
    },
  },
});

function componentNameOf(instance: ComponentPublicInstance | null): string | undefined {
  if (!instance) return undefined;
  const optionsName = instance.$options?.name;
  if (typeof optionsName === "string" && optionsName) return optionsName;
  const type = instance.$?.type as { name?: string; __name?: string } | undefined;
  return type?.name || type?.__name || undefined;
}

export default (app: App) => {
  app.use(VueQueryPlugin, { queryClient });

  // Every island's App passes through this Astro appEntrypoint, so this
  // is the single place to catch Vue render/lifecycle errors. Vue's
  // default handler (console + rethrow-in-dev) is replaced, so keep the
  // console.error for devtools and funnel a sanitized copy to Faro.
  app.config.errorHandler = (err, instance, info) => {
    console.error(err);
    reportVueError(err, componentNameOf(instance), info);
  };
};
