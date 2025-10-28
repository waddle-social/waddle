# Data Model: Waddle Chat Clickdummy

This clickdummy uses local state only. These entities define TypeScript interfaces used in the UI.

## Entities

### User
- id: string
- username: string
- displayName: string
- avatar?: string
- joinedAt: number (epoch ms)

### Message
- id: string
- userId: string
- username: string
- content: string
- timestamp: number (epoch ms)
- category: 'General' | 'Support' | 'Tech' | 'Gaming' | string
- replyTo?: string (Message.id)
- attachments?: Attachment[]

### Attachment
- id: string
- type: 'image' | 'video' | 'audio' | 'document' | 'link'
- url: string
- fileName?: string
- fileSize?: number
- mimeType?: string

### ChatRoom (local-only)
- id: 'global'
- messages: Message[]
- usersOnline: number

## Relationships
- User 1..* Message
- Message (optional) -> Message via replyTo

## Validation Rules
- Message.content: non-empty, trimmed length ≤ 2000 chars
- Username: ≥ 2 chars, alphanumeric plus `_` and `-`
- Category: one of predefined set or custom string; show badge accordingly

## UI State Machines (optional)
- Auth state: idle → authenticating → authenticated
- Chat connection (simulated): disconnected → connecting → connected

