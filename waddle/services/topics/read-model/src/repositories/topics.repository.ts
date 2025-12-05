import type { D1Database, D1PreparedStatement } from "@cloudflare/workers-types";
import type { Buffer } from "node:buffer";

export type TopicsFilter = {
  ownerId?: string | null;
  waddleSlug?: string | null;
};

export type TopicsPagination = {
  first?: number | null;
  after?: string | null;
};

export type TopicRecord = {
  id: string;
  title: string;
  description: string | null;
  scope: "global" | "owner" | "waddle";
  ownerId: string | null;
  waddleSlug: string | null;
  createdAt: number;
  updatedAt: number;
};

export type TopicsEdge = {
  cursor: string;
  node: TopicRecord;
};

export type TopicsConnection = {
  edges: TopicsEdge[];
  totalCount: number;
  pageInfo: {
    hasNextPage: boolean;
    endCursor: string | null;
  };
};

const toBase64 = (value: string) => {
  if (typeof globalThis.btoa === "function") {
    return globalThis.btoa(value);
  }

  const buffer = (globalThis as unknown as { Buffer?: typeof Buffer }).Buffer;
  if (buffer) {
    return buffer.from(value, "utf-8").toString("base64");
  }

  throw new Error("Base64 encoding not supported in this environment");
};

const fromBase64 = (value: string) => {
  if (typeof globalThis.atob === "function") {
    return globalThis.atob(value);
  }

  const buffer = (globalThis as unknown as { Buffer?: typeof Buffer }).Buffer;
  if (buffer) {
    return buffer.from(value, "base64").toString("utf-8");
  }

  throw new Error("Base64 decoding not supported in this environment");
};

const encodeCursor = (input: { createdAt: number; id: string }) =>
  toBase64(JSON.stringify(input));

const decodeCursor = (cursor: string): { createdAt: number; id: string } =>
  JSON.parse(fromBase64(cursor)) as { createdAt: number; id: string };

const MAX_PAGE_SIZE = 100;
const DEFAULT_PAGE_SIZE = 50;

const createWhereClause = (filter: TopicsFilter) => {
  const whereParts: string[] = [];
  const values: Array<string> = [];

  if (filter.ownerId) {
    whereParts.push("(scope = 'owner' AND owner_id = ?)");
    values.push(filter.ownerId);
  }

  if (filter.waddleSlug) {
    whereParts.push("(scope = 'waddle' AND waddle_slug = ?)");
    values.push(filter.waddleSlug);
  }

  const clause = whereParts.join(" OR ");
  return { clause, values } as const;
};

const bindValues = (statement: D1PreparedStatement, values: unknown[]) =>
  statement.bind(...values);

const buildPagination = (pagination: TopicsPagination) => {
  const pageSize = Math.min(
    Math.max(pagination.first ?? DEFAULT_PAGE_SIZE, 1),
    MAX_PAGE_SIZE,
  );

  if (!pagination.after) {
    return {
      pageSize,
      cursorClause: "",
      cursorValues: [] as unknown[],
    };
  }

  const { createdAt, id } = decodeCursor(pagination.after);

  return {
    pageSize,
    cursorClause: "AND (created_at < ? OR (created_at = ? AND id < ?))",
    cursorValues: [createdAt, createdAt, id] as unknown[],
  };
};

const buildTopicsConnection = (
  rows: TopicRecord[],
  totalCount: number,
  pageSize: number,
): TopicsConnection => {
  const hasNextPage = rows.length > pageSize;
  const slicedRows = hasNextPage ? rows.slice(0, pageSize) : rows;

  const edges = slicedRows.map((row) => ({
    cursor: encodeCursor({ createdAt: row.createdAt, id: row.id }),
    node: row,
  }));

  const endCursor = edges.at(-1)?.cursor ?? null;

  return {
    edges,
    totalCount,
    pageInfo: {
      hasNextPage,
      endCursor,
    },
  };
};

export const createTopicsRepository = (db: D1Database) => ({
  async getTopics(
    filter: TopicsFilter,
    pagination: TopicsPagination,
  ): Promise<TopicsConnection> {
    if (!filter.ownerId && !filter.waddleSlug) {
      throw new Error("Topics filter requires ownerId or waddleSlug");
    }

    if (filter.ownerId && filter.waddleSlug) {
      throw new Error("Provide either ownerId or waddleSlug, not both");
    }

    const { pageSize, cursorClause, cursorValues } = buildPagination(pagination);
    const { clause, values: whereValues } = createWhereClause(filter);

    const baseSelect = `
      SELECT
        id,
        title,
        description,
        scope,
        owner_id AS ownerId,
        waddle_slug AS waddleSlug,
        created_at AS createdAt,
        updated_at AS updatedAt
      FROM topics
      WHERE ${clause}
    `;

    const countStatement = bindValues(
      db.prepare(`SELECT COUNT(*) as count FROM topics WHERE ${clause}`),
      whereValues,
    );

    const totalCount = (await countStatement.first<number>("count")) ?? 0;

    const query = `${baseSelect}
      ${cursorClause}
      ORDER BY created_at DESC, id DESC
      LIMIT ?
    `;

    const rowsResult = await bindValues(
      db.prepare(query),
      [...whereValues, ...cursorValues, pageSize + 1],
    ).all<TopicRecord>();
    const rows = rowsResult.results ?? [];

    return buildTopicsConnection(rows, totalCount, pageSize);
  },
  async getAllTopics(
    pagination: TopicsPagination,
  ): Promise<TopicsConnection> {
    const { pageSize, cursorClause, cursorValues } = buildPagination(pagination);

    const totalCount =
      (await db.prepare("SELECT COUNT(*) as count FROM topics").first<number>("count")) ?? 0;

    const rowsResult = await bindValues(
      db.prepare(
        `SELECT
           id,
           title,
           description,
           scope,
           owner_id AS ownerId,
           waddle_slug AS waddleSlug,
           created_at AS createdAt,
           updated_at AS updatedAt
         FROM topics
         WHERE 1 = 1
         ${cursorClause}
         ORDER BY created_at DESC, id DESC
         LIMIT ?`,
      ),
      [...cursorValues, pageSize + 1],
    ).all<TopicRecord>();

    const rows = rowsResult.results ?? [];

    return buildTopicsConnection(rows, totalCount, pageSize);
  },
});
