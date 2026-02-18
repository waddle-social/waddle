-- Migration: Add embeds column to messages table
ALTER TABLE messages ADD COLUMN embeds TEXT;
