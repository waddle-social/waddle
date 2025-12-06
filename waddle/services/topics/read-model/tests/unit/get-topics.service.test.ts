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
    .bind("topic-global", "Global Topic", null, "global", null, null, now, now)
    .run();
  await insert
    .bind("topic-owner", "Owner Topic", null, "owner", "user-1", null, now, now)
    .run();
  await insert
    .bind("topic-waddle", "Waddle Topic", null, "waddle", null, "waddle-1", now, now)
    .run();
};

beforeEach(async () => {
  const db = await createSqlJsD1Database();
  env = { DB: db } as Env;
  await seedTopics();
});

afterEach(async () => {
  // sql.js is in-memory; nothing to dispose
});

describe("createTopicsRepository", () => {
  it("filters by owner", async () => {
    const repo = createTopicsRepository(env.DB);

    const result = await repo.getTopics({ ownerId: "user-1" }, { first: 10 });

    expect(result.totalCount).toBe(1);
    expect(result.edges[0].node.id).toBe("topic-owner");
  });

  it("filters by waddle", async () => {
    const repo = createTopicsRepository(env.DB);

    const result = await repo.getTopics({ waddleSlug: "waddle-1" }, { first: 10 });

    expect(result.totalCount).toBe(1);
    expect(result.edges[0].node.id).toBe("topic-waddle");
  });
});
