/// <reference types="astro/client" />

type D1Database = import('@cloudflare/workers-types').D1Database;

declare namespace App {
	interface Locals {
		runtime: {
			env: {
				DB: D1Database;
			};
		};
	}
}
