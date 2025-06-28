import type { User } from "@workos-inc/node";

declare global {
  namespace App {
    interface Locals extends Cloudflare.Env {
      user?: User;
    }
  }
}
