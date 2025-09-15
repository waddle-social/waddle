# Colony Authentication with Bluesky OAuth

This application implements authentication using Bluesky's OAuth flow with BetterAuth and Cloudflare D1.

## Setup Instructions

### 1. Install Dependencies

```bash
bun install
```

### 2. Generate Bluesky OAuth Keys

Generate ES256 key pair for Bluesky OAuth:

```bash
bun run generate-keys
```

Save the output:
- Copy the **private key** JSON to your `.env` file as `BLUESKY_PRIVATE_KEY`
- The **public key** will be served automatically via `/jwks.json` endpoint

### 3. Setup Cloudflare D1 Database

Create the D1 database:

```bash
wrangler d1 create colony-auth-db
```

Update `wrangler.toml` with the database ID from the output.

### 4. Run Database Migrations

Generate migration files:

```bash
bun run db:generate
```

Apply migrations locally:

```bash
bun run db:migrate:local
```

For production:

```bash
bun run db:migrate:remote
```

### 5. Environment Variables

Create a `.env` file based on `.env.example` and set:

```env
# Required for OAuth (private_key_jwt)
ATPROTO_PRIVATE_KEY='{"kty":"EC","use":"sig","alg":"ES256","kid":"your-key-id","crv":"P-256","x":"your-x","y":"your-y","d":"your-private-d"}'

# Required for Better Auth cookie/token signing
BETTER_AUTH_SECRET="replace-with-strong-secret"
```

For Cloudflare production, set these as Secrets (or Secret Store bindings) matching the same names.

### 6. Development

Run the development server:

```bash
bun run dev
```

The application will be available at `http://localhost:4321`

## Authentication Flow

1. User enters their Bluesky handle on the login page
2. Application initiates OAuth flow with Bluesky
3. User authorizes the application on Bluesky
4. Application receives OAuth callback with user data
5. User session is created in D1 database
6. User is redirected to dashboard

## Project Structure

```
src/
├── lib/
│   ├── auth/
│   │   ├── better-auth.ts      # BetterAuth configuration
│   │   ├── bluesky-oauth.ts    # Bluesky OAuth client
│   │   └── keys.ts              # Key generation utilities
│   └── db/
│       └── schema.ts            # Database schema
├── pages/
│   ├── api/
│   │   └── auth/
│   │       ├── bluesky.ts       # OAuth initiation
│   │       ├── bluesky/
│   │       │   └── callback.ts  # OAuth callback
│   │       ├── session.ts       # Session management
│   │       └── logout.ts        # Logout endpoint
│   ├── index.astro              # Login page
│   ├── dashboard.astro          # Authenticated dashboard
│   └── jwks.json.ts             # JWKS endpoint
└── env.d.ts                     # TypeScript environment types
```

## OAuth Endpoints

- `/client-metadata.json` - OAuth client metadata
- `/jwks.json` - Public key endpoint
- `/api/auth/bluesky` - Initiate OAuth flow
- `/api/auth/bluesky/callback` - Handle OAuth callback

## Security Notes

- Private keys are stored as environment variables
- Sessions are stored in D1 database with expiration
- CSRF protection via state parameter
- DPoP-bound access tokens for enhanced security

## Production Deployment

1. Set environment variables in Cloudflare dashboard
2. Run database migrations on production
3. Update OAuth URLs in `client-metadata.json` for production domain
4. Deploy with `wrangler deploy`
