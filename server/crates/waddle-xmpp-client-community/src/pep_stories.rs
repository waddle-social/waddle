//! XEP-0501 account stories and XEP-0470 reactions shared by the native and browser clients.

use std::collections::BTreeMap;

use jid::BareJid;
use minidom::Element;
use serde::Serialize;
use uuid::Uuid;
use waddle_xmpp_client::discovery::{build_roster_get_iq, parse_roster_result};
use waddle_xmpp_client::push::CommandDriver;
use waddle_xmpp_client::story_reactions::{
    build_own_story_attachment_iq, build_story_attachment_publish_iq,
    build_story_attachment_retract_iq, build_story_reaction_summary_iq, parse_own_story_attachment,
    parse_story_reaction_summary,
};
use waddle_xmpp_client::{ClientError, Subscription};
use waddle_xmpp_core::xep0470::{Attachment, AttachmentPayload, ReactionSet};

use crate::stories::{
    build_stories_items_iq, parse_stories_items_result, prepare_publication, WaddleStory,
    WaddleStoryInput,
};

#[derive(Debug, thiserror::Error)]
pub enum StoryError {
    #[error(transparent)]
    Client(#[from] ClientError),
    #[error("unexpected story reply sender")]
    Untrusted,
    #[error("invalid story input or response")]
    Invalid,
}

#[derive(Debug, Clone, Serialize)]
pub struct WaddleStoryReactions {
    pub counts: BTreeMap<String, u32>,
    pub mine: Vec<String>,
}

async fn query(
    driver: &impl CommandDriver,
    host: &BareJid,
    iq: Element,
) -> Result<Element, StoryError> {
    let result = driver.send_iq(iq).await?;
    if result
        .attr("from")
        .is_some_and(|from| from != host.as_str())
    {
        return Err(StoryError::Untrusted);
    }
    Ok(result)
}

fn or_absent(result: Result<Element, StoryError>) -> Result<Option<Element>, StoryError> {
    match result {
        Err(StoryError::Client(ClientError::StanzaError(err)))
            if err.condition == "item-not-found" =>
        {
            Ok(None)
        }
        other => other.map(Some),
    }
}

pub async fn authors(
    driver: &impl CommandDriver,
    own: &BareJid,
) -> Result<Vec<String>, StoryError> {
    let result = query(driver, own, build_roster_get_iq(None, None)).await?;
    let roster = parse_roster_result(&result).ok_or(StoryError::Invalid)?;
    let mut authors = vec![own.to_string()];
    authors.extend(
        roster
            .items
            .into_iter()
            .filter(|item| {
                matches!(item.subscription, Subscription::To | Subscription::Both)
                    && item.jid != *own
            })
            .map(|item| item.jid.to_string()),
    );
    Ok(authors)
}

pub async fn fetch(
    driver: &impl CommandDriver,
    author: &BareJid,
    own: &BareJid,
    max: Option<u32>,
) -> Result<Vec<WaddleStory>, StoryError> {
    let result = match or_absent(query(driver, author, build_stories_items_iq(author, max)).await) {
        Err(StoryError::Client(ClientError::StanzaError(err)))
            if author != own
                && matches!(
                    err.condition.as_str(),
                    "forbidden"
                        | "not-authorized"
                        | "service-unavailable"
                        | "feature-not-implemented"
                        | "remote-server-not-found"
                        | "remote-server-timeout"
                ) =>
        {
            None
        }
        other => other?,
    };
    let mut items = result
        .as_ref()
        .map(parse_stories_items_result)
        .unwrap_or_default();
    items.retain(|item| item.media_url.is_some());
    for item in &mut items {
        item.author = Some(author.to_string());
    }
    Ok(items)
}

pub async fn publish(
    driver: &impl CommandDriver,
    own: &BareJid,
    mut input: WaddleStoryInput,
) -> Result<WaddleStory, StoryError> {
    input.author = Some(own.to_string());
    let (request, story) = prepare_publication(None, input).map_err(|_| StoryError::Invalid)?;
    query(driver, own, request).await?;
    Ok(story)
}

async fn own_attachment(
    driver: &impl CommandDriver,
    author: &BareJid,
    id: &str,
    own: &BareJid,
) -> Result<Option<Attachment>, StoryError> {
    if id.trim().is_empty() {
        return Err(StoryError::Invalid);
    }
    let iq = build_own_story_attachment_iq(&Uuid::new_v4().to_string(), author, id, own);
    Ok(or_absent(query(driver, author, iq).await)?
        .and_then(|result| parse_own_story_attachment(&result, own)))
}

pub async fn reactions(
    driver: &impl CommandDriver,
    author: &BareJid,
    id: &str,
    own: &BareJid,
) -> Result<WaddleStoryReactions, StoryError> {
    let current = own_attachment(driver, author, id, own).await?;
    let iq = build_story_reaction_summary_iq(&Uuid::new_v4().to_string(), author, id);
    let counts = or_absent(query(driver, author, iq).await)?
        .and_then(|result| parse_story_reaction_summary(&result, id))
        .map(|summary| {
            summary
                .reactions
                .into_iter()
                .map(|r| (r.emoji, r.count))
                .collect()
        })
        .unwrap_or_default();
    let mine = current
        .and_then(|item| item.reactions_set().map(|set| set.emojis.clone()))
        .unwrap_or_default();
    Ok(WaddleStoryReactions { counts, mine })
}

pub async fn set_reactions(
    driver: &impl CommandDriver,
    author: &BareJid,
    id: &str,
    own: &BareJid,
    emojis: Vec<String>,
) -> Result<(), StoryError> {
    let reactions = ReactionSet::new(emojis);
    reactions.validate().map_err(|_| StoryError::Invalid)?;
    let current = own_attachment(driver, author, id, own).await?;
    let existed = current.is_some();
    let mut next = current.unwrap_or_else(Attachment::empty);
    next.payloads
        .retain(|payload| !matches!(payload, AttachmentPayload::Reactions(_)));
    if !reactions.is_empty() {
        next.payloads
            .insert(0, AttachmentPayload::Reactions(reactions));
    }
    let iq_id = Uuid::new_v4().to_string();
    if !next.payloads.is_empty() || !next.unknown_children.is_empty() {
        query(
            driver,
            author,
            build_story_attachment_publish_iq(&iq_id, author, id, own, &next),
        )
        .await?;
    } else if existed {
        or_absent(
            query(
                driver,
                author,
                build_story_attachment_retract_iq(&iq_id, author, id, own),
            )
            .await,
        )?;
    }
    Ok(())
}
