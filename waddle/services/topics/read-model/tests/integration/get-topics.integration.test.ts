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

const createRequest = (
  query: string,
  variables: Record<string, unknown> | undefined,
  headers: Record<string, string> = {},
) =>
  new Request("http://localhost/graphql", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      ...headers,
    },
    body: JSON.stringify({ query, variables }),
  });

const execute = async (
  query: string,
  variables: Record<string, unknown> | undefined,
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
  try {
    return JSON.parse(bodyText);
  } catch (error) {
    throw new Error(`Failed to parse response: ${bodyText}\n${error}`);
  }
};

const seedTopics = async () => {
  const now = Date.now();
  await env.DB.exec(migrationSql);
  await env.DB.prepare(
    "INSERT INTO topics (id, title, description, scope, owner_id, waddle_slug, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
  )
    .bind("topic-owner", "Owner Topic", null, "owner", "user-123", null, now, now)
    .run();
  await env.DB.prepare(
    "INSERT INTO topics (id, title, description, scope, owner_id, waddle_slug, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
  )
    .bind(
      "topic-waddle",
      "Waddle Topic",
      null,
      "waddle",
      null,
      "waddle-123",
      now,
      now,
    )
    .run();
};

beforeEach(async () => {
  const db = await createSqlJsD1Database();
  env = { DB: db } as Env;
  await seedTopics();
});

afterEach(async () => {
  // sql.js runs in-memory; nothing to dispose between tests
});

describe("getTopics integration", () => {
  it("returns topics scoped by waddle", async () => {
    const result = await execute(
      `#graphql
        query GetTopics($slug: ID!) {
          getTopics(filter: { waddleSlug: $slug }) {
            edges {
              node {
                id
                title
              }
            }
          }
        }
      `,
      { slug: "waddle-123" },
    );

    expect(result.errors).toBeUndefined();
    expect(result.data.getTopics.edges.map((edge: any) => edge.node.id)).toEqual([
      "topic-waddle",
    ]);
  });

  it("returns topics scoped by owner", async () => {
    const result = await execute(
      `#graphql
        query GetTopics($owner: ID!) {
          getTopics(filter: { ownerId: $owner }) {
            edges {
              node {
                id
                title
              }
            }
          }
        }
      `,
      { owner: "user-123" },
    );

    expect(result.errors).toBeUndefined();
    expect(result.data.getTopics.edges.map((edge: any) => edge.node.id)).toEqual([
      "topic-owner",
    ]);
  });
});
