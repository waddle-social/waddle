import type { App } from "vue";
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

export default (app: App) => {
  app.use(VueQueryPlugin, { queryClient });

  // Every island's App passes through this Astro appEntrypoint, so this
  // is the single place to catch Vue render/lifecycle errors. Vue's
  // default handler (console + rethrow-in-dev) is replaced, so keep the
  // console.error for devtools and send only a canonical code to Faro.
  app.config.errorHandler = (err) => {
    console.error(err);
    reportVueError();
  };
};
