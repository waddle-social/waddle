import initSqlJs, { Database, Statement } from "sql.js";
import type { D1Database, D1Result, D1PreparedStatement } from "@cloudflare/workers-types";

type SqlJsModule = Awaited<ReturnType<typeof initSqlJs>>;

class SqlJsPreparedStatement implements D1PreparedStatement {
  readonly statement: string;
  private readonly dbStatement: Statement;
  params: unknown[] = [];

  constructor(statement: string, dbStatement: Statement) {
    this.statement = statement;
    this.dbStatement = dbStatement;
  }

  bind(...values: any[]): SqlJsPreparedStatement {
    this.params = values;
    this.dbStatement.bind(values);
    return this;
  }

  async first<T = unknown>(colName?: string): Promise<T | null> {
    this.dbStatement.reset();
    this.dbStatement.bind(this.params);
    const hasRow = this.dbStatement.step();
    if (!hasRow) {
      this.dbStatement.reset();
      return null;
    }

    const row = this.dbStatement.getAsObject();
    this.dbStatement.reset();

    if (!colName) {
      const value = Object.values(row)[0] as T | undefined;
      return value ?? null;
    }

    return (row[colName as keyof typeof row] as T) ?? null;
  }

  async run<T = unknown>(): Promise<D1Result<T>> {
    this.dbStatement.reset();
    this.dbStatement.bind(this.params);
    this.dbStatement.step();
    this.dbStatement.reset();

    return {
      success: true,
      meta: {},
    } as D1Result<T>;
  }

  async all<T = unknown>(): Promise<D1Result<T>> {
    this.dbStatement.reset();
    this.dbStatement.bind(this.params);
    const results: unknown[] = [];
    while (this.dbStatement.step()) {
      results.push(this.dbStatement.getAsObject());
    }
    this.dbStatement.reset();

    return {
      results: results as T[],
      success: true,
      meta: {},
    } satisfies D1Result<T>;
  }

  async raw<T = unknown>(): Promise<T[]> {
    const { results } = await this.all<T>();
    return results ?? [];
  }
}

class SqlJsD1Database implements D1Database {
  constructor(private readonly sql: SqlJsModule, private readonly db: Database) {}

  prepare(query: string): SqlJsPreparedStatement {
    const statement = this.db.prepare(query);
    return new SqlJsPreparedStatement(query, statement);
  }

  async batch<T = unknown>(statements: SqlJsPreparedStatement[]): Promise<D1Result<T>[]> {
    const results: D1Result<T>[] = [];
    for (const statement of statements) {
      results.push(await statement.run<T>());
    }
    return results;
  }

  async exec<T = unknown>(query: string): Promise<D1Result<T>> {
    this.db.exec(query);
    return {
      success: true,
      meta: {},
    } as D1Result<T>;
  }

  dump(): Promise<ArrayBuffer> {
    return Promise.resolve(this.db.export().buffer as ArrayBuffer);
  }
}

export const createSqlJsD1Database = async () => {
  const sql = await initSqlJs();
  const database = new sql.Database();
  const d1 = new SqlJsD1Database(sql, database);
  return d1 as D1Database;
};
