# RFC-003: GraphQL Schema Design

**Status:** Proposed

**Author:** System

**Created:** 2025-09-30

## Abstract

This RFC defines the GraphQL schema for Waddle's federated architecture, using Pothos Schema Builder. The schema is split across multiple feature workers, composed via Apollo Federation.

## Schema Structure

### Shared Types (Colony - Identity Service)

```graphql
type User @key(fields: "id") {
  id: ID!
  did: String!
  handle: String!
  displayName: String
  avatar: String
  profile: AtProtoProfile
  createdAt: DateTime!
}

type AtProtoProfile {
  displayName: String
  description: String
  avatar: String
  banner: String
}

extend type Query {
  me: User
  user(id: ID!): User
  users(ids: [ID!]!): [User!]!
}
```

### Chat Schema (Waddle Worker)

```graphql
type Waddle @key(fields: "id") {
  id: ID!
  name: String!
  displayName: String!
  description: String
  iconUrl: String
  bannerUrl: String
  owner: User!
  isPublic: Boolean!
  memberCount: Int!
  members(limit: Int, offset: Int): [WaddleMember!]!
  messages(limit: Int, before: DateTime): [Message!]!
  conversations(limit: Int): [Conversation!]!
  integrations: [Integration!]!
  createdAt: DateTime!
}

type WaddleMember {
  user: User!
  waddle: Waddle!
  role: String!
  joinedAt: DateTime!
}

type Message @key(fields: "id") {
  id: ID!
  waddle: Waddle!
  author: User!
  content: String!
  rawContent: String!
  thread: Thread
  tags: [MessageTag!]!
  conversations: [Conversation!]!
  reactions: [Reaction!]!
  attachments: [Attachment!]!
  createdAt: DateTime!
  editedAt: DateTime
  deletedAt: DateTime
}

type MessageTag {
  tag: String!
  source: TagSource!
  confidence: Float
}

enum TagSource {
  HUMAN
  AI
}

type Conversation @key(fields: "id") {
  id: ID!
  waddle: Waddle!
  title: String
  tags: [String!]!
  messages(limit: Int): [Message!]!
  participants: [User!]!
  messageCount: Int!
  startedAt: DateTime!
  lastMessageAt: DateTime!
}

type Thread {
  id: ID!
  rootMessage: Message!
  messages: [Message!]!
  messageCount: Int!
}

type Reaction {
  emoji: String!
  users: [User!]!
  count: Int!
}

type Attachment {
  id: ID!
  type: AttachmentType!
  url: String!
  filename: String!
  size: Int!
  metadata: JSON
}

enum AttachmentType {
  IMAGE
  VIDEO
  AUDIO
  DOCUMENT
}

extend type User @key(fields: "id") {
  waddles: [Waddle!]!
  messages(waddleId: ID!, limit: Int): [Message!]!
}

extend type Query {
  waddle(id: ID!): Waddle
  waddles(userId: ID): [Waddle!]!
  message(id: ID!): Message
  conversation(id: ID!): Conversation
}

extend type Mutation {
  createWaddle(input: CreateWaddleInput!): Waddle!
  joinWaddle(waddleId: ID!): WaddleMember!
  leaveWaddle(waddleId: ID!): Boolean!

  sendMessage(input: SendMessageInput!): Message!
  editMessage(id: ID!, content: String!): Message!
  deleteMessage(id: ID!): Boolean!

  addReaction(messageId: ID!, emoji: String!): Reaction!
  removeReaction(messageId: ID!, emoji: String!): Boolean!
}

extend type Subscription {
  messageAdded(waddleId: ID!): Message!
  presenceUpdated(waddleId: ID!): PresenceUpdate!
  typingIndicator(waddleId: ID!): TypingIndicator!
}

input CreateWaddleInput {
  name: String!
  displayName: String!
  description: String
  isPublic: Boolean
}

input SendMessageInput {
  waddleId: ID!
  content: String!
  threadId: ID
  attachments: [AttachmentInput!]
}

input AttachmentInput {
  type: AttachmentType!
  url: String!
  filename: String!
  size: Int!
}

type PresenceUpdate {
  user: User!
  status: PresenceStatus!
  lastSeen: DateTime!
}

enum PresenceStatus {
  ONLINE
  IDLE
  OFFLINE
}

type TypingIndicator {
  users: [User!]!
  channelId: ID
}
```

### Views Schema (Views Worker)

```graphql
type View @key(fields: "id") {
  id: ID!
  user: User!
  waddle: Waddle!
  name: String!
  description: String
  icon: String
  isDefault: Boolean!
  sortOrder: SortOrder!
  groupBy: GroupBy!
  filters: [ViewFilter!]!
  createdAt: DateTime!
}

enum SortOrder {
  NEWEST
  OLDEST
  RELEVANCE
}

enum GroupBy {
  CONVERSATION
  TIME
  USER
  TAG
}

type ViewFilter {
  type: FilterType!
  operator: FilterOperator!
  value: JSON!
}

enum FilterType {
  TAG
  USER
  CONTENT
  TIME
  CHANNEL
}

enum FilterOperator {
  INCLUDES
  EXCLUDES
  EQUALS
  MATCHES
}

type SharedView @key(fields: "id") {
  id: ID!
  waddle: Waddle!
  name: String!
  description: String
  createdBy: User!
  isOfficial: Boolean!
  usageCount: Int!
  filters: [ViewFilter!]!
}

extend type User @key(fields: "id") {
  views(waddleId: ID!): [View!]!
}

extend type Waddle @key(fields: "id") {
  sharedViews: [SharedView!]!
}

extend type Query {
  view(id: ID!): View
  myViews(waddleId: ID!): [View!]!
  sharedViews(waddleId: ID!): [SharedView!]!
}

extend type Mutation {
  createView(input: CreateViewInput!): View!
  updateView(id: ID!, input: UpdateViewInput!): View!
  deleteView(id: ID!): Boolean!
  setDefaultView(id: ID!): View!

  createSharedView(input: CreateSharedViewInput!): SharedView!
}

input CreateViewInput {
  waddleId: ID!
  name: String!
  description: String
  sortOrder: SortOrder
  groupBy: GroupBy
  filters: [ViewFilterInput!]
}

input UpdateViewInput {
  name: String
  description: String
  sortOrder: SortOrder
  groupBy: GroupBy
  filters: [ViewFilterInput!]
}

input ViewFilterInput {
  type: FilterType!
  operator: FilterOperator!
  value: JSON!
}

input CreateSharedViewInput {
  waddleId: ID!
  name: String!
  description: String
  filters: [ViewFilterInput!]!
}
```

### Integration Schema (Integration Worker)

```graphql
type Integration @key(fields: "id") {
  id: ID!
  waddle: Waddle!
  type: IntegrationType!
  name: String!
  config: JSON!
  enabled: Boolean!
  createdBy: User!
  lastSync: DateTime
  lastError: String
  createdAt: DateTime!
}

enum IntegrationType {
  RSS
  YOUTUBE
  GITHUB
}

type IntegrationItem {
  id: ID!
  integration: Integration!
  externalId: String!
  type: String!
  title: String!
  url: String!
  description: String
  imageUrl: String
  publishedAt: DateTime!
  postedMessage: Message
}

extend type Waddle @key(fields: "id") {
  integrations: [Integration!]!
}

extend type Query {
  integration(id: ID!): Integration
  integrationItems(integrationId: ID!, limit: Int): [IntegrationItem!]!
}

extend type Mutation {
  createIntegration(input: CreateIntegrationInput!): Integration!
  updateIntegration(id: ID!, input: UpdateIntegrationInput!): Integration!
  deleteIntegration(id: ID!): Boolean!
  toggleIntegration(id: ID!, enabled: Boolean!): Integration!
}

input CreateIntegrationInput {
  waddleId: ID!
  type: IntegrationType!
  name: String!
  config: JSON!
}

input UpdateIntegrationInput {
  name: String
  config: JSON
}
```

## Pothos Implementation Example

```typescript
// chat-worker/schema/waddle.ts
import { builder } from '../builder';

export const Waddle = builder.objectRef<WaddleType>('Waddle').implement({
  fields: (t) => ({
    id: t.exposeID('id'),
    name: t.exposeString('name'),
    displayName: t.exposeString('displayName'),
    description: t.exposeString('description', { nullable: true }),
    iconUrl: t.exposeString('iconUrl', { nullable: true }),
    isPublic: t.exposeBoolean('isPublic'),
    memberCount: t.exposeInt('memberCount'),

    owner: t.field({
      type: User,
      resolve: (waddle) => ({ id: waddle.ownerId }),
    }),

    members: t.field({
      type: [WaddleMember],
      args: {
        limit: t.arg.int({ defaultValue: 50 }),
        offset: t.arg.int({ defaultValue: 0 }),
      },
      resolve: async (waddle, args, ctx) => {
        return ctx.loaders.waddleMembers.load({
          waddleId: waddle.id,
          limit: args.limit,
          offset: args.offset,
        });
      },
    }),

    messages: t.field({
      type: [Message],
      args: {
        limit: t.arg.int({ defaultValue: 50 }),
        before: t.arg({ type: 'DateTime', required: false }),
      },
      resolve: async (waddle, args, ctx) => {
        const db = await getWaddleDb(waddle.id, ctx.env);
        return await db.prepare(`
          SELECT * FROM messages
          WHERE deleted_at IS NULL
            ${args.before ? 'AND created_at < ?' : ''}
          ORDER BY created_at DESC
          LIMIT ?
        `).bind(...(args.before ? [args.before, args.limit] : [args.limit])).all();
      },
    }),

    conversations: t.field({
      type: [Conversation],
      args: { limit: t.arg.int({ defaultValue: 50 }) },
      resolve: async (waddle, args, ctx) => {
        const db = await getWaddleDb(waddle.id, ctx.env);
        return await db.prepare(`
          SELECT * FROM conversations
          ORDER BY last_message_at DESC
          LIMIT ?
        `).bind(args.limit).all();
      },
    }),

    createdAt: t.field({
      type: 'DateTime',
      resolve: (waddle) => waddle.createdAt,
    }),
  }),
});

// Mutations
builder.mutationField('createWaddle', (t) =>
  t.field({
    type: Waddle,
    args: {
      input: t.arg({ type: CreateWaddleInput }),
    },
    resolve: async (parent, args, ctx) => {
      if (!ctx.user) {
        throw new Error('Unauthorized');
      }

      const waddleId = crypto.randomUUID();
      // Create waddle...
      return { id: waddleId, ...args.input };
    },
  })
);

builder.mutationField('sendMessage', (t) =>
  t.field({
    type: Message,
    args: {
      input: t.arg({ type: SendMessageInput }),
    },
    resolve: async (parent, args, ctx) => {
      if (!ctx.user) {
        throw new Error('Unauthorized');
      }

      const messageId = await createMessage(
        args.input.waddleId,
        ctx.user.id,
        args.input.content,
        ctx.env
      );

      return { id: messageId, ...args.input };
    },
  })
);

// Subscriptions
builder.subscriptionField('messageAdded', (t) =>
  t.field({
    type: Message,
    args: {
      waddleId: t.arg.id(),
    },
    subscribe: (parent, args, ctx) => {
      return ctx.pubsub.subscribe(`waddle:${args.waddleId}:messages`);
    },
    resolve: (payload) => payload,
  })
);
```

## Federation Directives

Use Apollo Federation directives for type composition:

- `@key`: Define entity keys for type resolution
- `@shareable`: Mark fields that can be resolved by multiple subgraphs
- `@external`: Reference types from other subgraphs
- `@requires`: Specify required fields from other subgraphs

## References

- [Pothos GraphQL](https://pothos-graphql.dev/)
- [Apollo Federation](https://www.apollographql.com/docs/federation/)
- [GraphQL Yoga](https://the-guild.dev/graphql/yoga-server)