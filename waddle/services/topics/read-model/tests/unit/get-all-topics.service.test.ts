import { afterEach, beforeEach, describe, expect, it } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import type { Env } from "../../src";
import { createTopicsRepository } from "../../src/repositories/topics.repository";
import { createSqlJsD1Database } from "../utils/sql-js-d1";

const migrationSql = readFileSync(
  join(__dirname, "../../../data-model/migrations/0000_lyrical_karen_page.sql"),
  "utf-8",
);

let env: Env;

const seedTopics = async () => {
  const now = Date.now();
  await env.DB.exec(migrationSql);
  const insert = env.DB.prepare(
    "INSERT INTO topics (id, title, description, scope, owner_id, waddle_slug, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
  );
  await insert
    .bind("topic-one", "One", null, "global", null, null, now, now)
    .run();
  await insert
    .bind("topic-two", "Two", null, "owner", "user-123", null, now - 10, now - 10)
    .run();
  await insert
    .bind("topic-three", "Three", null, "waddle", null, "waddle-1", now - 20, now - 20)
    .run();
};

beforeEach(async () => {
  const db = await createSqlJsD1Database();
  env = { DB: db } as Env;
  await seedTopics();
});

afterEach(async () => {
  // sql.js cleanup not required
});

describe("createTopicsRepository.getAllTopics", () => {
  it("returns topics ordered by recency", async () => {
    const repo = createTopicsRepository(env.DB);

    const result = await repo.getAllTopics({ first: 10 });

    expect(result.totalCount).toBe(3);
    const ids = result.edges.map((edge) => edge.node.id);
    expect(ids).toEqual(["topic-one", "topic-two", "topic-three"]);
  });
});
