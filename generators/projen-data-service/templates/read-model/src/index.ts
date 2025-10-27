import { createYoga } from "graphql-yoga";
import type { D1Database, ExecutionContext } from "@cloudflare/workers-types";
import { getSchema } from "./schema";

export interface Env {
	DB: D1Database;
}

export default {
	async fetch(request: Request, env: Env, ctx: ExecutionContext) {
		const yoga = createYoga({
			schema: getSchema(env),
			graphqlEndpoint: "/",
			fetchAPI: {
				Response,
			},
		});

		return yoga.fetch(request, env, ctx);
	},
};
