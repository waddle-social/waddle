# Huddle Website

## Purpose

Main user-facing interface for hosts and guests to manage bookings, set availability, and connect calendars. Built with Astro for optimal performance and SEO.

## Architecture

- **Framework**: Astro with TypeScript
- **Styling**: Tailwind CSS
- **Auth**: ATProto OAuth
- **API**: Server actions and endpoints
- **Deployment**: Cloudflare Pages

## Pages Structure

### Public Pages

#### / (index.astro)
Landing page with value proposition.
- Hero section
- How it works
- Features overview
- CTA to sign up

#### /auth/login.astro
ATProto authentication flow.
- Handle/DID input
- PDS discovery
- OAuth redirect

#### /auth/callback.astro
OAuth callback handler.
- Token exchange
- Session creation
- Redirect to dashboard

### Protected Pages

#### /dashboard/index.astro
Main dashboard overview.
- Upcoming bookings
- Recent activity
- Quick actions
- Calendar status

#### /dashboard/offers.astro
Manage office hours.
- Create/edit slot offers
- Set availability windows
- Configure policies (mutual/follower/anyone)
- Timezone settings

#### /dashboard/connectors.astro
Calendar connections.
- Add Google Calendar
- Add Microsoft Outlook
- Manage permissions
- Sync status

#### /dashboard/bookings.astro
Booking management.
- Pending requests
- Confirmed bookings
- Past meetings
- Cancellation/rescheduling

#### /book/[handle].astro
Public booking page.
- Host's available slots
- Guest constraint input
- Request submission
- Turnstile verification

#### /booking/[id].astro
Booking status page.
- Current status
- Meeting details
- Add to calendar
- Cancellation option

## API Routes

### /api/auth/
Authentication endpoints.

#### login.ts
```typescript
export async function POST({ request }: APIContext) {
  const { handle } = await request.json()
  // Resolve DID
  // Generate OAuth URL
  // Return redirect URL
}
```

#### callback.ts
```typescript
export async function GET({ request, cookies }: APIContext) {
  const code = new URL(request.url).searchParams.get('code')
  // Exchange code for tokens
  // Create session
  // Set cookie
  // Redirect to dashboard
}
```

#### logout.ts
```typescript
export async function POST({ cookies }: APIContext) {
  // Clear session
  // Revoke tokens
  // Redirect to home
}
```

### /api/xrpc/
Proxy to AppView worker.

#### [...path].ts
```typescript
export async function ALL({ request, params }: APIContext) {
  const path = params.path
  return fetch(`${APPVIEW_URL}/xrpc/${path}`, {
    method: request.method,
    headers: request.headers,
    body: request.body
  })
}
```

### /api/turnstile.ts
Verify Turnstile challenges.

```typescript
export async function POST({ request, clientAddress }: APIContext) {
  const { token } = await request.json()
  // Verify with Cloudflare
  // Return success/failure
}
```

## Components

### Calendar Components
- `<WeekView />` - Weekly availability grid
- `<DayPicker />` - Date selection
- `<TimeSlotPicker />` - Time slot selection
- `<TimezoneSelector />` - Timezone dropdown

### Booking Components
- `<BookingCard />` - Booking summary card
- `<BookingForm />` - Constraint input form
- `<BookingStatus />` - Status indicator
- `<BookingActions />` - Cancel/reschedule buttons

### Offer Components
- `<OfferEditor />` - Create/edit offers
- `<OfferList />` - Display offers
- `<PolicySelector />` - Choose access policy
- `<RecurrencePattern />` - Set recurring availability

### Layout Components
- `<Header />` - Navigation and user menu
- `<Footer />` - Links and legal
- `<Sidebar />` - Dashboard navigation
- `<PageLayout />` - Common page wrapper

## Lib Modules

### auth.ts
Authentication utilities.
```typescript
export async function getSession(cookies: AstroCookies)
export async function requireAuth(context: APIContext)
export async function resolveDID(handle: string)
```

### api.ts
API client for AppView.
```typescript
export class AppViewClient {
  async match(request: MatchRequest)
  async finalize(bookingId: string)
  async listOffers(hostDid: string)
  async createOffer(offer: SlotOffer)
}
```

### calendar.ts
Calendar utilities.
```typescript
export function formatTimeSlot(start: Date, end: Date)
export function getTimezones()
export function convertToUTC(date: Date, tz: string)
```

## Styling

### Tailwind Config
```javascript
// tailwind.config.mjs
export default {
  content: ['./src/**/*.{astro,html,js,jsx,md,mdx,svelte,ts,tsx,vue}'],
  theme: {
    extend: {
      colors: {
        primary: {...},
        secondary: {...}
      }
    }
  },
  plugins: [
    require('@tailwindcss/forms'),
    require('@tailwindcss/typography')
  ]
}
```

### Design System
- Consistent spacing scale
- Color palette for states
- Typography hierarchy
- Component variants
- Dark mode support

## Configuration

### astro.config.mjs
```javascript
import { defineConfig } from 'astro/config'
import tailwind from '@astrojs/tailwind'
import cloudflare from '@astrojs/cloudflare'

export default defineConfig({
  output: 'server',
  adapter: cloudflare(),
  integrations: [tailwind()],
  vite: {
    define: {
      'import.meta.env.APPVIEW_URL': JSON.stringify(process.env.APPVIEW_URL),
      'import.meta.env.TURNSTILE_SITE_KEY': JSON.stringify(process.env.TURNSTILE_SITE_KEY)
    }
  }
})
```

## Security

- CSRF protection on forms
- Content Security Policy headers
- Secure session cookies
- Input sanitization
- Rate limiting via Cloudflare

## Performance

- Static asset optimization
- Image optimization with Cloudflare
- Code splitting by route
- Prefetching critical resources
- Edge caching strategy

## SEO

- Meta tags for social sharing
- Structured data for events
- Sitemap generation
- Robots.txt configuration
- Open Graph tags

## Accessibility

- ARIA labels and roles
- Keyboard navigation
- Screen reader support
- Color contrast compliance
- Focus indicators

## Testing

```bash
# Unit tests
bun test

# Component tests
bun test:components

# E2E tests
bun test:e2e

# Accessibility tests
bun test:a11y
```

## Development

```bash
# Install dependencies
bun install

# Start dev server
bun dev

# Build for production
bun build

# Preview production build
bun preview

# Deploy to Cloudflare Pages
wrangler pages deploy dist
```

## Environment Variables

```env
# .env
APPVIEW_URL=http://localhost:8787
TURNSTILE_SITE_KEY=...
PUBLIC_SITE_URL=https://huddle.waddle.social
```

## Deployment

### Cloudflare Pages
1. Connect GitHub repository
2. Set build command: `bun run build`
3. Set output directory: `dist`
4. Configure environment variables
5. Deploy on push to main

## Monitoring

- Real User Monitoring (RUM)
- Core Web Vitals tracking
- Error tracking with Sentry
- Analytics with Cloudflare
- Performance budgets

## Future Enhancements

- Progressive Web App (PWA)
- Offline support
- Real-time updates via WebSocket
- Mobile app with Capacitor
- Internationalization (i18n)
- Advanced calendar views
- Bulk booking management