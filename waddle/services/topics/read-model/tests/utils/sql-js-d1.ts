import { Database, type Statement } from "bun:sqlite";
import type { D1Database, D1Result, D1PreparedStatement } from "@cloudflare/workers-types";

class BunSqlitePreparedStatement implements D1PreparedStatement {
  readonly statement: string;
  private readonly dbStatement: Statement;
  private params: unknown[] = [];

  constructor(statement: string, dbStatement: Statement) {
    this.statement = statement;
    this.dbStatement = dbStatement;
  }

  bind(...values: any[]): BunSqlitePreparedStatement {
    this.params = values;
    return this;
  }

  async first<T = unknown>(colName?: string): Promise<T | null> {
    const row = this.dbStatement.get(...this.params) as Record<string, unknown> | undefined;

    if (!row) {
      return null;
    }

    if (!colName) {
      const value = Object.values(row)[0] as T | undefined;
      return value ?? null;
    }

    return (row[colName as keyof typeof row] as T) ?? null;
  }

  async run<T = unknown>(): Promise<D1Result<T>> {
    this.dbStatement.run(...this.params);
    return {
      success: true,
      meta: {},
    } as D1Result<T>;
  }

  async all<T = unknown>(): Promise<D1Result<T>> {
    const results = this.dbStatement.all(...this.params) as T[];
    return {
      results,
      success: true,
      meta: {},
    } satisfies D1Result<T>;
  }

  async raw<T = unknown>(): Promise<T[]> {
    const { results } = await this.all<T>();
    return results ?? [];
  }
}

class BunSqliteD1Database implements D1Database {
  constructor(private readonly db: Database) {}

  prepare(query: string): BunSqlitePreparedStatement {
    const statement = this.db.prepare(query);
    return new BunSqlitePreparedStatement(query, statement);
  }

  async batch<T = unknown>(statements: BunSqlitePreparedStatement[]): Promise<D1Result<T>[]> {
    const results: D1Result<T>[] = [];
    for (const statement of statements) {
      results.push(await statement.run<T>());
    }
    return results;
  }

  async exec<T = unknown>(query: string): Promise<D1Result<T>> {
    this.db.run(query);
    return {
      success: true,
      meta: {},
    } as D1Result<T>;
  }

  dump(): Promise<ArrayBuffer> {
    const buffer = this.db.serialize();
    return Promise.resolve(buffer.buffer as ArrayBuffer);
  }
}

export const createSqlJsD1Database = async () => {
  const db = new Database(":memory:");
  return new BunSqliteD1Database(db) as D1Database;
};
