/// <reference types="@cloudflare/workers-types" />

export interface Env {
	// Waddle services
	TOPICS: Fetcher;
	WADDLE: Fetcher;
	// Identity service for auth validation (add when auth service exists)
	// IDENTITY: Fetcher;
}
