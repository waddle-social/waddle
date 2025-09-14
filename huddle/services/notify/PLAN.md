# Notify Worker

## Purpose

Handle all notification delivery including email with ICS attachments, calendar invites, and future ATProto DM integration.

## Architecture

- **Queue Consumer**: Process notify-tasks queue
- **Multi-Channel**: Email, calendar, future DMs
- **Template Engine**: Dynamic content generation
- **Delivery Tracking**: Monitor notification success

## Migrations

### 0001_notification_log.sql
```sql
CREATE TABLE notification_log (
  id TEXT PRIMARY KEY,
  booking_id TEXT NOT NULL REFERENCES bookings(id),
  channel TEXT NOT NULL CHECK (channel IN ('email','calendar','dm')),
  recipient_type TEXT NOT NULL CHECK (recipient_type IN ('host','guest')),
  recipient_id TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('pending','sent','failed','bounced')),
  template TEXT NOT NULL,
  sent_at INTEGER,
  error_message TEXT,
  retry_count INTEGER DEFAULT 0,
  created_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE INDEX idx_notification_pending 
  ON notification_log(status, created_at) 
  WHERE status = 'pending';

CREATE TABLE notification_preferences (
  user_id TEXT PRIMARY KEY REFERENCES users(id),
  email_enabled BOOLEAN DEFAULT true,
  calendar_enabled BOOLEAN DEFAULT true,
  dm_enabled BOOLEAN DEFAULT false,
  email_address TEXT,
  timezone TEXT DEFAULT 'UTC',
  updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);
```

## Core Components

### index.ts
Queue consumer entry point.

```typescript
export default {
  async queue(batch: MessageBatch<NotifyTask>, env: Env) {
    for (const message of batch.messages) {
      await processNotification(message.body, env)
      message.ack()
    }
  }
}
```

### channels/email.ts

#### Email Provider
Using Cloudflare Email Workers or external service (SendGrid/Postmark).

#### Templates
- Booking confirmation
- Booking cancellation
- Booking reminder
- Hold expiration warning

#### ICS Attachment
```typescript
async function attachICS(email: Email, booking: Booking) {
  const ics = await env.R2_FILES.get(booking.ics_r2_key)
  email.attachments = [{
    filename: `booking-${booking.id}.ics`,
    content: await ics.text(),
    contentType: 'text/calendar'
  }]
}
```

### channels/calendar.ts

#### Calendar Invite
Direct calendar integration for confirmations.

```typescript
interface CalendarInvite {
  hostConnectorId: string
  guestConnectorId: string
  booking: Booking
  action: 'create' | 'update' | 'cancel'
}
```

#### Processing
1. Retrieve booking details
2. Get connector information
3. Create/update calendar event
4. Add attendees
5. Send via provider API

### channels/atproto-dm.ts (Future)

#### DM Integration
Send notifications via ATProto DMs when available.

```typescript
interface AtprotoDM {
  recipientDid: string
  message: string
  booking?: BookingReference
}
```

### templates/

#### Template Structure
```typescript
interface Template {
  subject: (data: any) => string
  html: (data: any) => string
  text: (data: any) => string
}
```

#### Available Templates
- `booking-confirmed` - Sent to both parties
- `booking-canceled` - Cancellation notice
- `booking-reminder` - 24 hours before
- `hold-expiring` - 5 minutes before expiry
- `connector-expired` - OAuth token expired

## Queue Messages

### NotifyTask Types

```typescript
type NotifyTask = 
  | BookingConfirmedTask
  | BookingCanceledTask
  | BookingReminderTask
  | HoldExpiringTask
  | ConnectorExpiredTask

interface BookingConfirmedTask {
  type: 'booking-confirmed'
  bookingId: string
  hostId: string
  guestId: string
  icsR2Key: string
}

interface BookingCanceledTask {
  type: 'booking-canceled'
  bookingId: string
  canceledBy: string
  reason?: string
}

interface BookingReminderTask {
  type: 'booking-reminder'
  bookingId: string
  hoursBefo re: number
}
```

## Bindings

```toml
# wrangler.toml
name = "huddle-notify"
main = "src/index.ts"
compatibility_date = "2025-09-14"

[vars]
EMAIL_FROM = "notifications@huddle.waddle.social"
EMAIL_PROVIDER = "cloudflare"  # or "sendgrid"
SENDGRID_API_KEY = "..."

[[d1_databases]]
binding = "DB"
database_name = "huddle-db"

[[r2_buckets]]
binding = "R2_FILES"
bucket_name = "huddle-files"

[[queues.consumers]]
queue = "notify-tasks"
max_batch_size = 50
max_retries = 3
dead_letter_queue = "notify-dlq"

[[queues.producers]]
binding = "Q_CALENDAR"
queue = "calendar-tasks"
```

## Email Delivery

### Cloudflare Email Workers
```typescript
async function sendViaCloudflare(email: Email) {
  // Use Cloudflare Email routing
  const response = await fetch('https://api.cloudflare.com/email/send', {
    method: 'POST',
    headers: {
      'Authorization': `Bearer ${env.CF_API_TOKEN}`,
      'Content-Type': 'application/json'
    },
    body: JSON.stringify(email)
  })
}
```

### External Provider (SendGrid)
```typescript
async function sendViaSendGrid(email: Email) {
  const sg = new SendGrid(env.SENDGRID_API_KEY)
  await sg.send(email)
}
```

## Notification Preferences

Users can configure:
- Channel preferences (email/calendar/DM)
- Email address override
- Timezone for scheduling
- Quiet hours
- Notification frequency

## Error Handling

- **Email Bounce**: Mark as bounced, don't retry
- **Invalid Address**: Log error, notify via alternative channel
- **Provider Error**: Exponential backoff retry
- **Template Error**: Use fallback template
- **ICS Not Found**: Send without attachment

## Performance

- Batch email sending where possible
- Cache templates in memory
- Pre-generate common ICS patterns
- Use queue batching for efficiency
- Implement circuit breakers for providers

## Monitoring

Key metrics:
- Delivery success rate by channel
- Average delivery time
- Bounce rate
- Template usage
- Queue processing time

## Testing

```bash
# Unit tests
bun test

# Template tests
bun test:templates

# Integration tests
bun test:integration

# Load tests
bun test:load
```

## Development

```bash
# Install dependencies
bun install

# Run migrations
wrangler d1 migrations apply DB

# Start dev server
wrangler dev

# Test email locally
bun run test:email

# Deploy
wrangler deploy
```

## Templates

### Booking Confirmation
```html
Subject: Booking Confirmed - {host.name} & {guest.name}

Hi {recipient.name},

Your booking has been confirmed!

Date: {booking.date}
Time: {booking.time} {booking.timezone}
Duration: {booking.duration} minutes
Location: {booking.location || 'Online'}

Add to calendar using the attached ICS file.

Best regards,
Huddle Team
```

### Reminder
```html
Subject: Reminder: Meeting tomorrow with {other.name}

Hi {recipient.name},

This is a reminder about your upcoming meeting:

Date: Tomorrow ({booking.date})
Time: {booking.time} {booking.timezone}
With: {other.name}

See you soon!
Huddle Team
```

## Security

- Sanitize all template inputs
- Validate email addresses
- Rate limit per recipient
- Implement unsubscribe links
- Never log PII in notifications

## Future Enhancements

- SMS notifications via Twilio
- Push notifications (web/mobile)
- Slack/Discord integration
- Rich HTML templates
- Notification bundling/digests
- Multi-language support
- A/B testing for templates