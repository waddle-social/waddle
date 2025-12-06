import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import type { ExecutionContext } from "@cloudflare/workers-types";
import type { Env } from "../../src";
import worker from "../../src";
import { createSqlJsD1Database } from "../utils/sql-js-d1";

const migrationSql = readFileSync(
  join(__dirname, "../../../data-model/migrations/0000_lyrical_karen_page.sql"),
  "utf-8",
);

let env: Env;

const createRequest = (query: string, variables?: Record<string, unknown>) =>
  new Request("http://localhost/", {
    method: "POST",
    headers: {
      "content-type": "application/json",
    },
    body: JSON.stringify({ query, variables }),
  });

const execute = async (
  query: string,
  variables?: Record<string, unknown>,
) => {
  const response = await worker.fetch(
    createRequest(query, variables),
    env,
    {
      waitUntil: () => {},
      passThroughOnException: () => {},
    } as ExecutionContext,
  );

  const bodyText = await response.text();
  if (!bodyText) {
    throw new Error(`Received empty response body (status ${response.status})`);
  }

  return JSON.parse(bodyText);
};

const seedTopics = async () => {
  const now = Date.now();
  await env.DB.exec(migrationSql);
  const insert = env.DB.prepare(
    "INSERT INTO topics (id, title, description, scope, owner_id, waddle_slug, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
  );
  await insert
    .bind("topic-global", "Global", null, "global", null, null, now, now)
    .run();
  await insert
    .bind("topic-owner", "Owner", null, "owner", "user-1", null, now, now)
    .run();
  await insert
    .bind("topic-waddle", "Waddle", null, "waddle", null, "waddle-1", now, now)
    .run();
};

beforeEach(async () => {
  const db = await createSqlJsD1Database();
  env = { DB: db } as Env;
  await seedTopics();
});

afterEach(async () => {
  // sql.js runs in-memory and does not require explicit teardown.
});

describe("getAllTopics integration", () => {
  it("returns all topics with pagination defaults", async () => {
    const result = await execute(
      `#graphql
        query GetAllTopics {
          getAllTopics {
            totalCount
            edges {
              node { id title scope }
            }
          }
        }
      `,
    );

    expect(result.errors).toBeUndefined();
    expect(result.data.getAllTopics.totalCount).toBe(3);
    expect(result.data.getAllTopics.edges).toHaveLength(3);
  });

  it("supports cursor pagination", async () => {
    const firstPage = await execute(
      `#graphql
        query GetAllTopics($first: Int!) {
          getAllTopics(pagination: { first: $first }) {
            pageInfo { endCursor hasNextPage }
            edges { cursor node { id } }
          }
        }
      `,
      { first: 2 },
    );

    expect(firstPage.errors).toBeUndefined();
    expect(firstPage.data.getAllTopics.pageInfo.hasNextPage).toBe(true);

    const secondPage = await execute(
      `#graphql
        query GetAllTopics($after: String!) {
          getAllTopics(pagination: { after: $after }) {
            edges { node { id } }
          }
        }
      `,
      { after: firstPage.data.getAllTopics.pageInfo.endCursor },
    );

    expect(secondPage.errors).toBeUndefined();
    const ids = secondPage.data.getAllTopics.edges.map((edge: any) => edge.node.id);
    expect(ids).toHaveLength(1);
    expect(firstPage.data.getAllTopics.edges[0].node.id).not.toEqual(ids[0]);
  });
});
