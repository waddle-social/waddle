-- redirect_urls are synchronized at runtime from WADDLE_SERVER_OIDC_REDIRECT_URLS.
DELETE FROM oauth_application
WHERE client_id = 'waddle-chat';

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
  'waddle-server',
  'Waddle Server',
  '{"firstParty":true,"product":"server"}',
  'waddle-server',
  '[]',
  'web',
  0,
  unixepoch() * 1000,
  unixepoch() * 1000
)
ON CONFLICT(client_id) DO NOTHING;
