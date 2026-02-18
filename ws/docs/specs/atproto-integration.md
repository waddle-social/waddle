# ATProto Integration Specification

## Overview

This document specifies how Waddle Social integrates with the AT Protocol (ATProto) for identity and Bluesky announcements.

## Identity Flow

### ATProto OAuth

Waddle uses ATProto OAuth for authentication, following the [OAuth for AT Protocol](https://atproto.com/specs/oauth) specification.

### Authorization Flow

```
┌────────┐                              ┌────────────┐                    ┌─────────┐
│  User  │                              │   Waddle   │                    │   PDS   │
└───┬────┘                              └─────┬──────┘                    └────┬────┘
    │                                         │                                │
    │ 1. Click "Login with Bluesky"           │                                │
    │────────────────────────────────────────>│                                │
    │                                         │                                │
    │                                         │ 2. Resolve handle to DID       │
    │                                         │───────────────────────────────>│
    │                                         │<───────────────────────────────│
    │                                         │                                │
    │                                         │ 3. Get authorization server    │
    │                                         │───────────────────────────────>│
    │                                         │<───────────────────────────────│
    │                                         │                                │
    │ 4. Redirect to PDS authorization        │                                │
    │<────────────────────────────────────────│                                │
    │                                         │                                │
    │ 5. User authorizes on PDS               │                                │
    │────────────────────────────────────────────────────────────────────────>│
    │                                         │                                │
    │ 6. Redirect with authorization code     │                                │
    │────────────────────────────────────────>│                                │
    │                                         │                                │
    │                                         │ 7. Exchange code for tokens    │
    │                                         │───────────────────────────────>│
    │                                         │<───────────────────────────────│
    │                                         │                                │
    │ 8. Login complete                       │                                │
    │<────────────────────────────────────────│                                │
```

### Implementation

#### 1. Start Authorization

```http
POST /v1/auth/atproto/authorize HTTP/1.1
Content-Type: application/json

{
  "handle": "alice.bsky.social"
}
```

Response:
```json
{
  "authorization_url": "https://bsky.social/oauth/authorize?...",
  "state": "random_state_value",
  "code_verifier": "pkce_verifier"  // Store for callback
}
```

#### 2. Handle Resolution

```rust
async fn resolve_handle(handle: &str) -> Result<Did> {
    // Try DNS TXT record first
    if let Ok(did) = resolve_dns_txt(handle).await {
        return Ok(did);
    }

    // Fall back to .well-known
    let url = format!("https://{}/.well-known/atproto-did", handle);
    let did = reqwest::get(&url).await?.text().await?;

    Ok(Did::parse(&did)?)
}
```

#### 3. DID Document Retrieval

```rust
async fn get_did_document(did: &Did) -> Result<DidDocument> {
    match did {
        Did::Plc(id) => {
            let url = format!("https://plc.directory/{}", did);
            Ok(reqwest::get(&url).await?.json().await?)
        }
        Did::Web(domain) => {
            let url = format!("https://{}/.well-known/did.json", domain);
            Ok(reqwest::get(&url).await?.json().await?)
        }
    }
}
```

#### 4. Authorization Server Discovery

From DID document, find the PDS service:

```json
{
  "@context": ["https://www.w3.org/ns/did/v1"],
  "id": "did:plc:abc123",
  "service": [
    {
      "id": "#atproto_pds",
      "type": "AtprotoPersonalDataServer",
      "serviceEndpoint": "https://bsky.social"
    }
  ]
}
```

Then fetch OAuth metadata:

```http
GET /.well-known/oauth-authorization-server HTTP/1.1
Host: bsky.social
```

#### 5. OAuth Callback

```http
GET /v1/auth/atproto/callback?code=xxx&state=yyy HTTP/1.1
```

Exchange code for tokens:

```rust
async fn exchange_code(code: &str, verifier: &str, pds: &str) -> Result<Tokens> {
    let token_endpoint = format!("{}/oauth/token", pds);

    let response = reqwest::Client::new()
        .post(&token_endpoint)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("code_verifier", verifier),
            ("client_id", &client_id()),
            ("redirect_uri", &redirect_uri()),
        ])
        .send()
        .await?;

    Ok(response.json().await?)
}
```

### Token Storage

```rust
struct UserSession {
    did: String,
    handle: String,
    pds_url: String,
    access_token: String,      // Short-lived
    refresh_token: String,     // Long-lived
    token_expires_at: DateTime<Utc>,
}
```

Tokens stored securely in database, encrypted at rest.

### Token Refresh

```rust
async fn refresh_tokens(session: &UserSession) -> Result<Tokens> {
    let token_endpoint = format!("{}/oauth/token", session.pds_url);

    let response = reqwest::Client::new()
        .post(&token_endpoint)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", &session.refresh_token),
            ("client_id", &client_id()),
        ])
        .send()
        .await?;

    Ok(response.json().await?)
}
```

## Data Separation

### What We Use from ATProto

| Data | Source | Usage |
|------|--------|-------|
| DID | ATProto | Primary user identifier |
| Handle | ATProto | Display name, mentions |
| Avatar | PDS/CDN | Profile display |
| Display Name | PDS | Profile display |

### What We Store Separately

| Data | Storage | Reason |
|------|---------|--------|
| Messages | Waddle DB | Performance, features |
| Channels | Waddle DB | Custom structure |
| Permissions | Waddle DB | ReBAC model |
| Presence | Waddle Memory | Real-time requirements |
| Files | Waddle S3 | Control, performance |

## Bluesky Announcements

### Posting to PDS

When broadcasting announcements to Bluesky:

```rust
async fn post_to_bluesky(
    session: &UserSession,
    content: &str,
    facets: Vec<Facet>,
) -> Result<StrongRef> {
    let agent = BskyAgent::new(session.pds_url.clone());
    agent.resume_session(session.access_token.clone()).await?;

    let record = AppBskyFeedPost {
        text: content.to_string(),
        facets: Some(facets),
        created_at: Utc::now().to_rfc3339(),
        ..Default::default()
    };

    let result = agent.api.com.atproto.repo.create_record(
        CreateRecordInput {
            repo: session.did.clone(),
            collection: "app.bsky.feed.post".to_string(),
            record: serde_json::to_value(record)?,
            ..Default::default()
        }
    ).await?;

    Ok(StrongRef {
        uri: result.uri,
        cid: result.cid,
    })
}
```

### Rich Text Facets

Convert Waddle message format to Bluesky facets:

```rust
fn convert_to_facets(content: &str, mentions: &[Mention]) -> (String, Vec<Facet>) {
    let mut facets = Vec::new();
    let mut text = content.to_string();

    for mention in mentions {
        if mention.mention_type == "user" {
            if let Some(handle) = resolve_did_to_handle(&mention.id) {
                let byte_start = text.find(&format!("@{}", mention.display))
                    .unwrap_or(0);
                let byte_end = byte_start + mention.display.len() + 1;

                facets.push(Facet {
                    index: ByteSlice { byte_start, byte_end },
                    features: vec![
                        FacetFeature::Mention(MentionFeature {
                            did: mention.id.clone(),
                        })
                    ],
                });
            }
        }
    }

    // Handle links
    for link in extract_links(&text) {
        facets.push(Facet {
            index: ByteSlice {
                byte_start: link.start,
                byte_end: link.end,
            },
            features: vec![
                FacetFeature::Link(LinkFeature {
                    uri: link.url.clone(),
                })
            ],
        });
    }

    (text, facets)
}
```

### Image Uploads

Upload images to PDS blob store:

```rust
async fn upload_image(
    session: &UserSession,
    image_data: &[u8],
    mime_type: &str,
) -> Result<BlobRef> {
    let agent = BskyAgent::new(session.pds_url.clone());
    agent.resume_session(session.access_token.clone()).await?;

    let result = agent.api.com.atproto.repo.upload_blob(
        image_data.to_vec(),
        mime_type.to_string(),
    ).await?;

    Ok(result.blob)
}
```

### Post with Images

```rust
async fn post_with_images(
    session: &UserSession,
    content: &str,
    images: Vec<ImageEmbed>,
) -> Result<StrongRef> {
    let mut image_refs = Vec::new();

    for image in images {
        let blob = upload_image(session, &image.data, &image.mime_type).await?;
        image_refs.push(AppBskyEmbedImagesImage {
            image: blob,
            alt: image.alt_text.unwrap_or_default(),
            aspect_ratio: image.aspect_ratio,
        });
    }

    let record = AppBskyFeedPost {
        text: content.to_string(),
        embed: Some(PostEmbed::Images(AppBskyEmbedImages {
            images: image_refs,
        })),
        created_at: Utc::now().to_rfc3339(),
        ..Default::default()
    };

    // ... create record
}
```

## Error Handling

### Common Errors

| Error | Handling |
|-------|----------|
| `ExpiredToken` | Refresh token and retry |
| `InvalidToken` | Re-authenticate user |
| `HandleNotFound` | Show error to user |
| `PDSUnavailable` | Retry with backoff |
| `RateLimited` | Queue and retry later |

### Retry Logic

```rust
async fn with_retry<T, F, Fut>(operation: F) -> Result<T>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let mut attempts = 0;
    let max_attempts = 3;

    loop {
        match operation().await {
            Ok(result) => return Ok(result),
            Err(e) if attempts < max_attempts && is_retryable(&e) => {
                attempts += 1;
                let delay = Duration::from_millis(100 * 2_u64.pow(attempts));
                tokio::time::sleep(delay).await;
            }
            Err(e) => return Err(e),
        }
    }
}
```

## Security Considerations

### Token Security

- Access tokens stored encrypted
- Refresh tokens rotated on use
- Tokens never logged
- HTTPS only for all ATProto communication

### PKCE

All OAuth flows use PKCE (Proof Key for Code Exchange):

```rust
fn generate_pkce() -> (String, String) {
    let verifier: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(64)
        .map(char::from)
        .collect();

    let challenge = BASE64_URL_SAFE_NO_PAD.encode(
        Sha256::digest(verifier.as_bytes())
    );

    (verifier, challenge)
}
```

### Scope Limitations

Request minimal scopes:

```
atproto transition:generic
```

Only request posting scope when user enables Bluesky announcements.

## Related

- [ADR-0005: ATProto Identity](../adrs/0005-atproto-identity.md)
- [RFC-0011: Bluesky Announcements](../rfcs/0011-bluesky-broadcast.md)
- [Spec: API Contracts](./api-contracts.md)
