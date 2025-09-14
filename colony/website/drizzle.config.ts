import { defineConfig } from 'drizzle-kit';

export default defineConfig({
  schema: './src/lib/db/schema.ts',
  out: './migrations',
  dialect: 'sqlite',
  dbCredentials: {
    wranglerConfigPath: 'wrangler.toml',
    dbName: 'colony',
  },
});