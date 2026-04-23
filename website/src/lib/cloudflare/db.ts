import { drizzle } from "drizzle-orm/d1";

import type { D1Database } from "@cloudflare/workers-types";

export function getWaitlistDb(database: D1Database) {
	return drizzle(database);
}
