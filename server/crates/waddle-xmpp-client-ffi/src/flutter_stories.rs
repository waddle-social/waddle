//! Flutter bindings for per-author PEP stories.

use jid::BareJid;
pub use waddle_xmpp_client_community::pep_stories::WaddleStoryReactions;
use waddle_xmpp_client_community::pep_stories::{self, StoryError};
pub use waddle_xmpp_client_community::stories::{WaddleStory, WaddleStoryInput};

use crate::error::client_error_to_waddle;
use crate::messaging_verbs::account_bare_jid;
use crate::notify_settings::TimeoutCommandDriver;
use crate::{WaddleClient, WaddleError};

fn story_error(error: StoryError) -> WaddleError {
    match error {
        StoryError::Client(e) => client_error_to_waddle(&e),
        StoryError::Invalid => WaddleError::InvalidArgument,
        StoryError::Untrusted => WaddleError::UntrustedReply,
    }
}

impl WaddleClient {
    async fn story_session(&self) -> Result<(TimeoutCommandDriver, BareJid), WaddleError> {
        let own = account_bare_jid(&self.config.jid).map_err(|_| WaddleError::InvalidJid)?;
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        Ok((TimeoutCommandDriver { handle }, own))
    }

    /// List the account and contacts whose presence it receives.
    pub async fn story_authors(&self) -> Result<Vec<String>, WaddleError> {
        let (driver, own) = self.story_session().await?;
        pep_stories::authors(&driver, &own)
            .await
            .map_err(story_error)
    }

    /// Read stories from one author’s PEP node.
    pub async fn fetch_stories(
        &self,
        author_jid: String,
        max_items: Option<u32>,
    ) -> Result<Vec<WaddleStory>, WaddleError> {
        let (driver, own) = self.story_session().await?;
        let author = self.require_bare_jid(&author_jid)?;
        pep_stories::fetch(&driver, &author, &own, max_items)
            .await
            .map_err(story_error)
    }

    /// Publish a story to this account’s PEP node.
    pub async fn publish_story(&self, input: WaddleStoryInput) -> Result<WaddleStory, WaddleError> {
        let (driver, own) = self.story_session().await?;
        pep_stories::publish(&driver, &own, input)
            .await
            .map_err(story_error)
    }

    /// Fetch aggregate reactions and this account’s emoji for one story.
    pub async fn fetch_story_reactions(
        &self,
        author_jid: String,
        story_id: String,
    ) -> Result<WaddleStoryReactions, WaddleError> {
        let (driver, own) = self.story_session().await?;
        let author = self.require_bare_jid(&author_jid)?;
        pep_stories::reactions(&driver, &author, &story_id, &own)
            .await
            .map_err(story_error)
    }

    /// Replace this account’s reaction set on one story.
    pub async fn set_story_reactions(
        &self,
        author_jid: String,
        story_id: String,
        emojis: Vec<String>,
    ) -> Result<(), WaddleError> {
        let (driver, own) = self.story_session().await?;
        let author = self.require_bare_jid(&author_jid)?;
        pep_stories::set_reactions(&driver, &author, &story_id, &own, emojis)
            .await
            .map_err(story_error)
    }
}

#[cfg(test)]
#[path = "flutter_stories_tests.rs"]
mod tests;
