-- Remove static first-party server client records.
-- Server clients are now created via dynamic registration.
DELETE FROM oauth_application
WHERE client_id = 'waddle-server';
