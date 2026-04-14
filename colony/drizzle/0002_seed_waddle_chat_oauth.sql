-- Seed the code-whitelisted Chat OIDC client so oauth_access_token foreign keys resolve.
-- redirect_urls are intentionally environment-agnostic in migrations.
INSERT INTO oauth_application (
  id,
  name,
  metadata,
  client_id,
  redirect_urls,
  type,
  disabled,
  created_at,
  updated_at
)
VALUES (
  'waddle-chat',
  'Waddle Chat',
  '{"firstParty":true,"product":"chat"}',
  'waddle-chat',
  '[]',
  'public',
  0,
  unixepoch() * 1000,
  unixepoch() * 1000
)
ON CONFLICT(client_id) DO NOTHING;
