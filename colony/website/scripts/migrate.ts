#!/usr/bin/env bun

import { drizzle } from 'drizzle-orm/d1';
import { migrate } from 'drizzle-orm/d1/migrator';

async function runMigrations() {
  console.log('🔄 Running database migrations...\n');
  
  try {
    // This will be run via wrangler d1 migrations
    console.log('📝 To create and run migrations:');
    console.log('1. Generate migration: bun drizzle-kit generate:sqlite');
    console.log('2. Apply migration: wrangler d1 migrations apply colony-auth-db --local');
    console.log('3. For production: wrangler d1 migrations apply colony-auth-db --remote');
    console.log('\n✅ Migration instructions complete!');
  } catch (error) {
    console.error('❌ Migration failed:', error);
    process.exit(1);
  }
}

runMigrations();