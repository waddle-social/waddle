//! The community feed and stories over the session's `WaddleClient`.
//!
//! Like `session.rs` this only adapts: wire records become the plain types
//! `feed.rs` and `stories.rs` work with. The server's feed and stories
//! live on `community.<domain>`.

use std::collections::HashMap;
use std::sync::Arc;

use waddle_client_session::Attachment;
use waddle_xmpp_client_ffi::{WaddleFeedPost, WaddleStoryPost, WaddleStoryRead};

use crate::attachments::{self, TransferError};
use crate::feed::{self, Post, PostDraft};
use crate::profile::Details;
use crate::runtime;
use crate::session::{Outgoing, Session};
use crate::session_profile::describe;
use crate::stories::{self, Reactions, Reads, Story};

/// Profiles looked up at once for author names.
const NAME_LOOKUPS: usize = 4;

impl Session {
    fn community(&self) -> String {
        feed::community_service(self.me.bare.domain().as_ref())
    }

    /// The community feed, newest first.
    pub async fn feed(&self) -> Result<Vec<Post>, String> {
        let (client, community) = (Arc::clone(&self.client), self.community());
        let entries =
            runtime::spawn(
                async move { client.fetch_feed(community, Some(feed::PAGE_SIZE)).await },
            )
            .await
            .map_err(|_| interrupted())?
            .map_err(|error| describe(&error))?;
        Ok(feed::newest_first(entries))
    }

    /// Post to the feed; the post comes back as the server will show it.
    pub async fn post_to_feed(&self, draft: PostDraft) -> Result<Post, String> {
        let (client, community) = (Arc::clone(&self.client), self.community());
        let post = WaddleFeedPost {
            title: None,
            body: draft.body,
            link: draft.link,
        };
        runtime::spawn(async move { client.publish_feed_post(community, post).await })
            .await
            .map_err(|_| interrupted())?
            .map(Post::from_wire)
            .map_err(|error| describe(&error))
    }

    /// The stories still up, in the server's order.
    pub async fn stories(&self, now_ms: i64) -> Result<Vec<Story>, String> {
        let (client, community) = (Arc::clone(&self.client), self.community());
        let wire =
            runtime::spawn(
                async move { client.fetch_stories(community, Some(feed::PAGE_SIZE)).await },
            )
            .await
            .map_err(|_| interrupted())?
            .map_err(|error| describe(&error))?;
        Ok(stories::active(wire, now_ms))
    }

    /// Share a picture already uploaded as a story that lasts the default
    /// day.
    pub async fn publish_story(
        &self,
        media_url: String,
        media_type: String,
        caption: Option<String>,
    ) -> Result<Story, String> {
        let (client, community) = (Arc::clone(&self.client), self.community());
        let post = WaddleStoryPost {
            body: caption,
            media_url,
            media_type: Some(media_type),
            expiry_hours: None,
        };
        runtime::spawn(async move { client.publish_story(community, post).await })
            .await
            .map_err(|_| interrupted())?
            .map(Story::from_wire)
            .map_err(|error| describe(&error))
    }

    /// Which stories this account has seen, on any device.
    pub async fn story_reads(&self) -> Result<Reads, String> {
        let client = Arc::clone(&self.client);
        runtime::spawn(async move { client.fetch_story_reads().await })
            .await
            .map_err(|_| interrupted())?
            .map(Reads::from_wire)
            .map_err(|error| describe(&error))
    }

    /// Replace the account's "seen" state with `reads`.
    pub async fn publish_story_reads(&self, reads: Vec<WaddleStoryRead>) -> Result<(), String> {
        let client = Arc::clone(&self.client);
        runtime::spawn(async move { client.publish_story_reads(reads).await })
            .await
            .map_err(|_| interrupted())?
            .map_err(|error| describe(&error))
    }

    /// The reactions on one story and which are this account's.
    pub async fn story_reactions(&self, story_id: String) -> Result<Reactions, String> {
        let (client, community) = (Arc::clone(&self.client), self.community());
        runtime::spawn(async move { client.fetch_story_reactions(community, story_id).await })
            .await
            .map_err(|_| interrupted())?
            .map(Reactions::from_wire)
            .map_err(|error| describe(&error))
    }

    /// Make `emojis` this account's reactions to the story.
    pub async fn set_story_reactions(
        &self,
        story_id: String,
        emojis: Vec<String>,
    ) -> Result<(), String> {
        let (client, community) = (Arc::clone(&self.client), self.community());
        runtime::spawn(async move {
            client
                .set_story_reactions(community, story_id, emojis)
                .await
        })
        .await
        .map_err(|_| interrupted())?
        .map_err(|error| describe(&error))
    }

    /// Upload a file as it is (XEP-0363) and return its address. For what
    /// the whole community reads: a story's picture or a post's. Chat
    /// attachments go through [`Session::upload`], which encrypts them.
    pub async fn upload_public(&self, file: Outgoing) -> Result<String, TransferError> {
        let client = Arc::clone(&self.client);
        let transfers = self.transfers.clone();
        runtime::spawn(async move {
            let size = u64::try_from(file.bytes.len()).map_err(|_| TransferError::TooLarge)?;
            if size == 0 {
                return Err(attachments::AttachmentError::Empty.into());
            }
            if size > attachments::MAX_UPLOAD_BYTES {
                return Err(attachments::AttachmentError::TooLarge.into());
            }
            let service = client
                .discover_upload_service()
                .await
                .ok_or(TransferError::NoUploadService)?;
            let slot = client
                .request_upload_slot(service, file.name.clone(), size, file.media_type.clone())
                .await
                .ok_or(TransferError::SlotRefused)?;
            // Everyone who reads the story must be able to fetch it.
            transfers.check(&slot.get_url)?;
            let headers: Vec<(String, String)> = slot
                .put_headers
                .iter()
                .map(|header| (header.name.clone(), header.value.clone()))
                .collect();
            transfers.put(&slot.put_url, &headers, file.bytes).await?;
            Ok(slot.get_url)
        })
        .await
        .map_err(|_| TransferError::Interrupted)?
    }

    /// A story's picture, fetched like any shared file.
    pub async fn fetch_public(&self, url: String) -> Result<Vec<u8>, TransferError> {
        self.fetch(Attachment {
            url,
            name: None,
            media_type: None,
            size: None,
            inline: true,
            encryption: None,
        })
        .await
    }

    /// Display names from the profiles of `authors`, for those that have
    /// one. A profile that cannot be read is left out, and the address
    /// stands in.
    pub async fn author_names(&self, authors: Vec<String>) -> HashMap<String, String> {
        let this = self.clone();
        runtime::spawn(async move {
            let mut names = HashMap::new();
            for chunk in authors.chunks(NAME_LOOKUPS) {
                let mut lookups = tokio::task::JoinSet::new();
                for author in chunk {
                    let (session, author) = (this.clone(), author.clone());
                    lookups.spawn(async move {
                        let vcard = session.vcard(author.clone()).await.ok()?;
                        let profile = Details::from_wire(vcard);
                        let name = profile.display_name("");
                        (!name.is_empty()).then_some((author, name))
                    });
                }
                while let Some(done) = lookups.join_next().await {
                    if let Ok(Some((author, name))) = done {
                        names.insert(author, name);
                    }
                }
            }
            names
        })
        .await
        .unwrap_or_default()
    }
}

fn interrupted() -> String {
    "The request stopped unexpectedly.".to_string()
}
