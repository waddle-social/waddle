import { beforeEach, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import worker from "../../src";
import type { Env } from "../../src";
import type { ExecutionContext } from "@cloudflare/workers-types";
import { createSqlJsD1Database } from "../utils/sql-js-d1";

const migrationSql = readFileSync(
  join(__dirname, "../../../data-model/migrations/0000_lyrical_karen_page.sql"),
  "utf-8",
);

let env: Env;

const execute = async (query: string, variables?: Record<string, unknown>) => {
  const response = await worker.fetch(
    new Request("http://localhost/graphql", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ query, variables }),
    }),
    env,
    {
      waitUntil: () => {},
      passThroughOnException: () => {},
    } as ExecutionContext,
  );

  const bodyText = await response.text();
  return JSON.parse(bodyText);
};

beforeEach(async () => {
  const db = await createSqlJsD1Database();
  env = { DB: db } as Env;

  await env.DB.exec(migrationSql);
  await env.DB.prepare(
    "INSERT INTO topics (id, title, scope, owner_id, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)",
  )
    .bind("topic-quickstart", "Quickstart", "owner", "user-demo", Date.now(), Date.now())
    .run();
});

describe("quickstart smoke", () => {
  it("returns data for sample queries", async () => {
    const scoped = await execute(
      `#graphql
        query QuickstartScoped($owner: ID!) {
          getTopics(filter: { ownerId: $owner }) {
            edges { node { id title } }
          }
        }
      `,
      { owner: "user-demo" },
    );

    expect(scoped.errors).toBeUndefined();
    expect(scoped.data.getTopics.edges[0].node.id).toBe("topic-quickstart");

    const all = await execute(
      `#graphql
        query QuickstartAll {
          getAllTopics {
            totalCount
          }
        }
      `,
    );

    expect(all.errors).toBeUndefined();
    expect(all.data.getAllTopics.totalCount).toBeGreaterThanOrEqual(1);
  });
});
