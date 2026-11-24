//! The community admin console: what to ask, how to validate it and when
//! to confirm, plus the adapter over `waddle-xmpp-client-ffi`'s
//! `urn:waddle:admin:*` commands.
//!
//! Everything but the `Session` adapter is plain data. `ui/admin.rs`
//! draws it. The server re-checks every command, so hiding the console
//! from non-owners ([`Session::admin_is_owner`]) is a courtesy, not a
//! security boundary.

use std::future::Future;
use std::sync::Arc;

use jid::BareJid;
use waddle_xmpp_client_ffi::{
    WaddleAdminChannelAffiliationEntry, WaddleAdminChannelListEntry,
    WaddleAdminChannelOccupantEntry, WaddleAdminChannelsAffiliationsArgs,
    WaddleAdminChannelsCreateArgs, WaddleAdminChannelsListArgs, WaddleAdminChannelsOccupantsArgs,
    WaddleAdminChannelsUpdateArgs, WaddleAdminSpaceEntry, WaddleAdminSpaceMemberEntry,
    WaddleAdminSpacesCreateArgs, WaddleAdminSpacesListArgs, WaddleAdminSpacesMembersArgs,
    WaddleAdminSpacesUpdateArgs, WaddleAdminUserEntry, WaddleClient, WaddleError,
    WaddleMucAffiliation, WaddleSpaceRole,
};

use crate::runtime;
use crate::session::Session;

/// Rows asked of the server per page.
pub const PAGE_SIZE: u32 = 50;

/// One page of rows and the cursor for the next.
pub type PageOf<T> = (Vec<T>, Option<String>);

// ── Errors ───────────────────────────────────────────────────────────

/// What to tell the owner when a command failed.
pub fn describe_error(error: &WaddleError) -> String {
    match error {
        WaddleError::Stanza { condition, text } => match condition.as_str() {
            "forbidden" | "not-allowed" => "Only the community owner can do this.".to_string(),
            "item-not-found" => "That no longer exists. Refresh and try again.".to_string(),
            "conflict" => "That name or address is already taken.".to_string(),
            "bad-request" | "not-acceptable" | "jid-malformed" => text
                .clone()
                .filter(|text| !text.trim().is_empty())
                .unwrap_or_else(|| "The server did not accept that.".to_string()),
            _ => text
                .clone()
                .filter(|text| !text.trim().is_empty())
                .unwrap_or_else(|| format!("The server refused it ({condition}).")),
        },
        WaddleError::NotConnected | WaddleError::Transport => {
            "You are offline. Try again when Waddle has reconnected.".to_string()
        }
        WaddleError::Timeout => "The server took too long to answer.".to_string(),
        WaddleError::InvalidJid => "That is not a valid address.".to_string(),
        WaddleError::InvalidArgument => "The server did not accept that.".to_string(),
        _ => "The server's answer could not be understood.".to_string(),
    }
}

// ── Validation ───────────────────────────────────────────────────────

/// A required, trimmed name.
pub fn required_name(text: &str) -> Result<String, &'static str> {
    let name = text.trim();
    if name.is_empty() {
        Err("A name is required.")
    } else {
        Ok(name.to_string())
    }
}

/// `Some(trimmed)` when `new` differs from what the server has.
fn changed(old: Option<&str>, new: &str) -> Option<String> {
    let new = new.trim();
    (new != old.unwrap_or("").trim()).then(|| new.to_string())
}

/// Blank optional text is not sent.
fn optional(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// A person's address, as the owner typed it.
pub fn member_jid(text: &str) -> Result<String, &'static str> {
    let text = text.trim();
    if text.is_empty() {
        return Err("An address is required.");
    }
    text.parse::<BareJid>()
        .map(|jid| jid.to_string())
        .map_err(|_| "That is not a valid address, like name@example.com.")
}

/// The address the server's kick command takes: the occupant's real
/// address without its resource.
pub fn kick_target(real_jid: &str) -> String {
    real_jid.split('/').next().unwrap_or(real_jid).to_string()
}

/// Whether `jid` (bare or full) is the signed-in account.
pub fn is_me(jid: &str, me: &BareJid) -> bool {
    let bare = jid.split('/').next().unwrap_or(jid);
    bare.eq_ignore_ascii_case(me.as_str())
}

// ── Space and channel forms ──────────────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpaceForm {
    pub name: String,
    pub description: String,
    pub icon_url: String,
}

impl SpaceForm {
    pub fn from_entry(entry: &WaddleAdminSpaceEntry) -> Self {
        Self {
            name: entry.name.clone(),
            description: entry.description.clone().unwrap_or_default(),
            icon_url: entry.icon_url.clone().unwrap_or_default(),
        }
    }

    pub fn create_args(&self) -> Result<WaddleAdminSpacesCreateArgs, &'static str> {
        Ok(WaddleAdminSpacesCreateArgs {
            name: required_name(&self.name)?,
            description: optional(&self.description),
            icon_url: optional(&self.icon_url),
        })
    }

    /// Only what changed; `None` when nothing did.
    pub fn update_args(
        &self,
        original: &WaddleAdminSpaceEntry,
    ) -> Result<Option<WaddleAdminSpacesUpdateArgs>, &'static str> {
        let name = required_name(&self.name)?;
        let args = WaddleAdminSpacesUpdateArgs {
            space_jid: original.space_jid.clone(),
            space_node: Some(original.space_node.clone()),
            name: changed(Some(&original.name), &name),
            description: changed(original.description.as_deref(), &self.description),
            icon_url: changed(original.icon_url.as_deref(), &self.icon_url),
        };
        let nothing = args.name.is_none() && args.description.is_none() && args.icon_url.is_none();
        Ok((!nothing).then_some(args))
    }
}

/// Channel kinds an owner can pick (`group_dm` is made by the people in
/// it, not by an admin).
pub const CHANNEL_TYPES: [(&str, &str); 3] = [
    ("text", "Text"),
    ("announcement", "Announcement"),
    ("forum", "Forum"),
];

/// The index of `wire` in [`CHANNEL_TYPES`] (text when unknown).
pub fn channel_type_index(wire: &str) -> u32 {
    CHANNEL_TYPES
        .iter()
        .position(|(value, _)| *value == wire)
        .map_or(0, |index| u32::try_from(index).unwrap_or(0))
}

pub fn channel_type_wire(index: u32) -> &'static str {
    usize::try_from(index)
        .ok()
        .and_then(|index| CHANNEL_TYPES.get(index))
        .map_or("text", |(value, _)| *value)
}

pub fn channel_type_label(wire: &str) -> String {
    CHANNEL_TYPES
        .iter()
        .find(|(value, _)| *value == wire)
        .map_or_else(
            || {
                let mut text = wire.replace('_', " ");
                if let Some(first) = text.get_mut(0..1) {
                    first.make_ascii_uppercase();
                }
                text
            },
            |(_, label)| (*label).to_string(),
        )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelForm {
    pub name: String,
    pub topic: String,
    /// The wire value of the kind.
    pub channel_type: String,
    pub is_public: bool,
    pub members_only: bool,
}

impl Default for ChannelForm {
    fn default() -> Self {
        Self {
            name: String::new(),
            topic: String::new(),
            channel_type: "text".to_string(),
            is_public: true,
            members_only: false,
        }
    }
}

impl ChannelForm {
    pub fn from_entry(entry: &WaddleAdminChannelListEntry) -> Self {
        Self {
            name: entry.name.clone(),
            topic: entry.topic.clone().unwrap_or_default(),
            channel_type: entry.channel_type.clone(),
            is_public: entry.is_public,
            members_only: entry.members_only,
        }
    }

    pub fn create_args(
        &self,
        space: &WaddleAdminSpaceEntry,
    ) -> Result<WaddleAdminChannelsCreateArgs, &'static str> {
        Ok(WaddleAdminChannelsCreateArgs {
            name: required_name(&self.name)?,
            topic: optional(&self.topic),
            channel_type: Some(self.channel_type.clone()),
            space_jid: Some(space.space_jid.clone()),
            space_node: Some(space.space_node.clone()),
            is_public: Some(self.is_public),
            members_only: Some(self.members_only),
        })
    }

    pub fn update_args(
        &self,
        original: &WaddleAdminChannelListEntry,
    ) -> Result<Option<WaddleAdminChannelsUpdateArgs>, &'static str> {
        let name = required_name(&self.name)?;
        let args = WaddleAdminChannelsUpdateArgs {
            channel_jid: original.channel_jid.clone(),
            name: changed(Some(&original.name), &name),
            topic: changed(original.topic.as_deref(), &self.topic),
            channel_type: (self.channel_type != original.channel_type)
                .then(|| self.channel_type.clone()),
            is_public: (self.is_public != original.is_public).then_some(self.is_public),
            members_only: (self.members_only != original.members_only).then_some(self.members_only),
        };
        let nothing = args.name.is_none()
            && args.topic.is_none()
            && args.channel_type.is_none()
            && args.is_public.is_none()
            && args.members_only.is_none();
        Ok((!nothing).then_some(args))
    }
}

// ── Roles and affiliations ───────────────────────────────────────────

/// Space roles in the order the picker lists them.
pub const SPACE_ROLES: [WaddleSpaceRole; 4] = [
    WaddleSpaceRole::Owner,
    WaddleSpaceRole::Admin,
    WaddleSpaceRole::Member,
    WaddleSpaceRole::None,
];

pub fn space_role_label(role: WaddleSpaceRole) -> &'static str {
    match role {
        WaddleSpaceRole::Owner => "Owner",
        WaddleSpaceRole::Admin => "Admin",
        WaddleSpaceRole::Member => "Member",
        WaddleSpaceRole::None => "Removed",
    }
}

pub fn space_role_index(role: WaddleSpaceRole) -> u32 {
    SPACE_ROLES
        .iter()
        .position(|candidate| *candidate == role)
        .map_or(0, |index| u32::try_from(index).unwrap_or(0))
}

pub fn space_role_at(index: u32) -> Option<WaddleSpaceRole> {
    SPACE_ROLES.get(usize::try_from(index).ok()?).copied()
}

/// Room affiliations in the order the picker lists them.
pub const AFFILIATIONS: [WaddleMucAffiliation; 5] = [
    WaddleMucAffiliation::Owner,
    WaddleMucAffiliation::Admin,
    WaddleMucAffiliation::Member,
    WaddleMucAffiliation::Outcast,
    WaddleMucAffiliation::None,
];

pub fn affiliation_label(affiliation: WaddleMucAffiliation) -> &'static str {
    match affiliation {
        WaddleMucAffiliation::Owner => "Owner",
        WaddleMucAffiliation::Admin => "Admin",
        WaddleMucAffiliation::Member => "Member",
        WaddleMucAffiliation::Outcast => "Banned",
        WaddleMucAffiliation::None => "No affiliation",
    }
}

pub fn affiliation_index(affiliation: WaddleMucAffiliation) -> u32 {
    AFFILIATIONS
        .iter()
        .position(|candidate| *candidate == affiliation)
        .map_or(0, |index| u32::try_from(index).unwrap_or(0))
}

pub fn affiliation_at(index: u32) -> Option<WaddleMucAffiliation> {
    AFFILIATIONS.get(usize::try_from(index).ok()?).copied()
}

pub fn role_names() -> Vec<&'static str> {
    SPACE_ROLES.iter().copied().map(space_role_label).collect()
}

pub fn affiliation_names() -> Vec<&'static str> {
    AFFILIATIONS
        .iter()
        .copied()
        .map(affiliation_label)
        .collect()
}

/// A change the owner must confirm: it removes someone, or may lock the
/// owner out of what they are changing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirmation {
    pub heading: String,
    pub body: String,
    pub action: &'static str,
}

/// `None` when the role change is routine.
pub fn confirm_space_role(
    member: &str,
    current: WaddleSpaceRole,
    new: WaddleSpaceRole,
    myself: bool,
) -> Option<Confirmation> {
    if current == new {
        return None;
    }
    if new == WaddleSpaceRole::None {
        return Some(Confirmation {
            heading: "Remove from Space?".to_string(),
            body: format!("{member} loses access to this space and its channels."),
            action: "Remove",
        });
    }
    if myself && space_role_rank(new) > space_role_rank(current) {
        return Some(Confirmation {
            heading: "Lower Your Own Role?".to_string(),
            body: format!(
                "You would become {} in this space and may not be able to undo it.",
                space_role_label(new).to_lowercase()
            ),
            action: "Change Role",
        });
    }
    None
}

/// Lower is more powerful.
fn space_role_rank(role: WaddleSpaceRole) -> u8 {
    match role {
        WaddleSpaceRole::Owner => 0,
        WaddleSpaceRole::Admin => 1,
        WaddleSpaceRole::Member => 2,
        WaddleSpaceRole::None => 3,
    }
}

/// `None` when the affiliation change is routine.
pub fn confirm_affiliation(
    member: &str,
    current: WaddleMucAffiliation,
    new: WaddleMucAffiliation,
    myself: bool,
) -> Option<Confirmation> {
    if current == new {
        return None;
    }
    match new {
        WaddleMucAffiliation::Outcast => Some(Confirmation {
            heading: "Ban from Channel?".to_string(),
            body: format!("{member} is removed and cannot rejoin until the ban is lifted."),
            action: "Ban",
        }),
        WaddleMucAffiliation::None if current != WaddleMucAffiliation::Outcast => {
            Some(Confirmation {
                heading: "Remove Affiliation?".to_string(),
                body: format!("{member} loses their standing in this channel."),
                action: "Remove",
            })
        }
        _ if myself
            && matches!(
                current,
                WaddleMucAffiliation::Owner | WaddleMucAffiliation::Admin
            ) =>
        {
            Some(Confirmation {
                heading: "Lower Your Own Standing?".to_string(),
                body: "You may not be able to undo this.".to_string(),
                action: "Change",
            })
        }
        _ => None,
    }
}

// ── Summaries and prompts ────────────────────────────────────────────

fn count(n: u32, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

pub fn space_summary(space: &WaddleAdminSpaceEntry) -> String {
    let counts = format!(
        "{} · {}",
        count(space.channel_count, "channel", "channels"),
        count(space.member_count, "member", "members")
    );
    match space.description.as_deref().map(str::trim) {
        Some(text) if !text.is_empty() => format!("{text}\n{counts}"),
        _ => counts,
    }
}

pub fn channel_summary(channel: &WaddleAdminChannelListEntry) -> String {
    let mut parts = vec![
        channel_type_label(&channel.channel_type),
        if channel.is_public {
            "Public"
        } else {
            "Private"
        }
        .to_string(),
    ];
    if channel.members_only {
        parts.push("Members only".to_string());
    }
    parts.push(count(channel.occupant_count, "occupant", "occupants"));
    parts.join(" · ")
}

pub fn delete_space_prompt(space: &WaddleAdminSpaceEntry) -> Confirmation {
    Confirmation {
        heading: format!("Delete {}?", space.name),
        body: format!(
            "This destroys the space and its {}, including their history. It cannot be undone.",
            count(space.channel_count, "channel", "channels")
        ),
        action: "Delete Space",
    }
}

pub fn delete_channel_prompt(channel: &WaddleAdminChannelListEntry) -> Confirmation {
    Confirmation {
        heading: format!("Delete #{}?", channel.name),
        body: "This destroys the channel and its history for everyone. It cannot be undone."
            .to_string(),
        action: "Delete Channel",
    }
}

pub fn kick_prompt(occupant: &WaddleAdminChannelOccupantEntry) -> Confirmation {
    Confirmation {
        heading: format!("Remove {} from the Room?", occupant.nick),
        body: "They are removed from the room now. Unless they are banned, they can rejoin."
            .to_string(),
        action: "Remove",
    }
}

pub fn occupant_summary(occupant: &WaddleAdminChannelOccupantEntry) -> String {
    format!(
        "{} · {}",
        occupant.real_jid,
        affiliation_label(occupant.affiliation)
    )
}

pub fn user_title(user: &WaddleAdminUserEntry) -> String {
    user.display_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map_or_else(|| user.jid.clone(), str::to_string)
}

// ── Paging ───────────────────────────────────────────────────────────

/// A list that is read page by page, with the load in flight.
#[derive(Debug)]
pub struct Paged<T> {
    pub items: Vec<T>,
    cursor: Option<String>,
    more: bool,
    loaded: bool,
    loading: bool,
    error: Option<String>,
    seq: u64,
}

impl<T> Default for Paged<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            cursor: None,
            more: false,
            loaded: false,
            loading: false,
            error: None,
            seq: 0,
        }
    }
}

/// A request that [`Paged::begin`] allowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    seq: u64,
    pub after: Option<String>,
}

impl<T> Paged<T> {
    /// Start a read. `restart` begins again from the first page and
    /// supersedes anything in flight; otherwise this asks for the next
    /// page, or nothing when there is none or one is already loading.
    pub fn begin(&mut self, restart: bool) -> Option<Ticket> {
        if restart {
            self.seq = self.seq.wrapping_add(1);
            self.items.clear();
            self.cursor = None;
            self.more = false;
            self.loaded = false;
        } else if self.loading || !self.more {
            if !self.loaded && !self.loading {
                // A first read that failed is retried from the start.
                return self.begin(true);
            }
            return None;
        }
        self.loading = true;
        self.error = None;
        Some(Ticket {
            seq: self.seq,
            after: self.cursor.clone(),
        })
    }

    /// Take a read's answer. False when it was superseded.
    pub fn finish(&mut self, ticket: &Ticket, result: Result<PageOf<T>, String>) -> bool {
        if ticket.seq != self.seq {
            return false;
        }
        self.loading = false;
        match result {
            Ok((rows, next)) => {
                // A server that repeats the cursor would loop forever.
                self.more = next.is_some() && next != self.cursor && !rows.is_empty();
                self.cursor = next;
                self.items.extend(rows);
                self.loaded = true;
            }
            Err(message) => self.error = Some(message),
        }
        true
    }

    pub fn has_more(&self) -> bool {
        self.more
    }

    pub fn is_loading(&self) -> bool {
        self.loading
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

// ── The session adapter ──────────────────────────────────────────────

impl Session {
    async fn admin_call<T, F, Fut>(&self, call: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(Arc<WaddleClient>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, WaddleError>> + Send + 'static,
    {
        let client = Arc::clone(&self.client);
        match runtime::spawn(async move { call(client).await }).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => {
                tracing::debug!(?error, "an admin command failed");
                Err(describe_error(&error))
            }
            Err(_) => Err("The request was interrupted.".to_string()),
        }
    }

    /// Whether the server lets this account use the admin commands.
    pub async fn admin_is_owner(&self) -> bool {
        let client = Arc::clone(&self.client);
        runtime::spawn(async move { client.is_community_owner().await })
            .await
            .unwrap_or(false)
    }

    pub async fn admin_spaces(
        &self,
        after: Option<String>,
    ) -> Result<PageOf<WaddleAdminSpaceEntry>, String> {
        let args = WaddleAdminSpacesListArgs {
            prefix: None,
            page_size: Some(PAGE_SIZE),
            after_cursor: after,
        };
        let page = self
            .admin_call(move |client| async move { client.admin_spaces_list(args).await })
            .await?;
        Ok((page.entries, page.next_cursor))
    }

    pub async fn admin_create_space(
        &self,
        args: WaddleAdminSpacesCreateArgs,
    ) -> Result<(), String> {
        self.admin_call(move |client| async move { client.admin_spaces_create(args).await })
            .await
            .map(|_| ())
    }

    pub async fn admin_update_space(
        &self,
        args: WaddleAdminSpacesUpdateArgs,
    ) -> Result<(), String> {
        self.admin_call(move |client| async move { client.admin_spaces_update(args).await })
            .await
            .map(|_| ())
    }

    pub async fn admin_delete_space(&self, space: &WaddleAdminSpaceEntry) -> Result<(), String> {
        let (jid, node) = (space.space_jid.clone(), space.space_node.clone());
        self.admin_call(
            move |client| async move { client.admin_spaces_delete(jid, Some(node)).await },
        )
        .await
    }

    pub async fn admin_space_members(
        &self,
        space: &WaddleAdminSpaceEntry,
        after: Option<String>,
    ) -> Result<PageOf<WaddleAdminSpaceMemberEntry>, String> {
        let args = WaddleAdminSpacesMembersArgs {
            space_jid: space.space_jid.clone(),
            space_node: Some(space.space_node.clone()),
            page_size: Some(PAGE_SIZE),
            after_cursor: after,
        };
        let page = self
            .admin_call(move |client| async move { client.admin_spaces_members(args).await })
            .await?;
        Ok((page.entries, page.next_cursor))
    }

    pub async fn admin_set_space_role(
        &self,
        space: &WaddleAdminSpaceEntry,
        member: String,
        role: WaddleSpaceRole,
    ) -> Result<(), String> {
        let (jid, node) = (space.space_jid.clone(), space.space_node.clone());
        self.admin_call(move |client| async move {
            client
                .admin_spaces_set_role(jid, Some(node), member, role)
                .await
        })
        .await
        .map(|_| ())
    }

    pub async fn admin_channels(
        &self,
        space: &WaddleAdminSpaceEntry,
        after: Option<String>,
    ) -> Result<PageOf<WaddleAdminChannelListEntry>, String> {
        let args = WaddleAdminChannelsListArgs {
            space_jid: Some(space.space_jid.clone()),
            space_node: Some(space.space_node.clone()),
            prefix: None,
            page_size: Some(PAGE_SIZE),
            after_cursor: after,
        };
        let page = self
            .admin_call(move |client| async move { client.admin_channels_list(args).await })
            .await?;
        Ok((page.entries, page.next_cursor))
    }

    pub async fn admin_create_channel(
        &self,
        args: WaddleAdminChannelsCreateArgs,
    ) -> Result<(), String> {
        self.admin_call(move |client| async move { client.admin_channels_create(args).await })
            .await
            .map(|_| ())
    }

    pub async fn admin_update_channel(
        &self,
        args: WaddleAdminChannelsUpdateArgs,
    ) -> Result<(), String> {
        self.admin_call(move |client| async move { client.admin_channels_update(args).await })
            .await
            .map(|_| ())
    }

    pub async fn admin_delete_channel(&self, channel_jid: String) -> Result<(), String> {
        self.admin_call(
            move |client| async move { client.admin_channels_delete(channel_jid).await },
        )
        .await
    }

    pub async fn admin_affiliations(
        &self,
        channel_jid: String,
        after: Option<String>,
    ) -> Result<PageOf<WaddleAdminChannelAffiliationEntry>, String> {
        let args = WaddleAdminChannelsAffiliationsArgs {
            channel_jid,
            filter: None,
            page_size: Some(PAGE_SIZE),
            after_cursor: after,
        };
        let page = self
            .admin_call(move |client| async move { client.admin_channels_affiliations(args).await })
            .await?;
        Ok((page.entries, page.next_cursor))
    }

    pub async fn admin_set_affiliation(
        &self,
        channel_jid: String,
        member: String,
        affiliation: WaddleMucAffiliation,
        reason: Option<String>,
    ) -> Result<(), String> {
        self.admin_call(move |client| async move {
            client
                .admin_channels_set_affiliation(channel_jid, member, affiliation, reason)
                .await
        })
        .await
        .map(|_| ())
    }

    pub async fn admin_occupants(
        &self,
        channel_jid: String,
        after: Option<String>,
    ) -> Result<PageOf<WaddleAdminChannelOccupantEntry>, String> {
        let args = WaddleAdminChannelsOccupantsArgs {
            channel_jid,
            page_size: Some(PAGE_SIZE),
            after_cursor: after,
        };
        let page = self
            .admin_call(move |client| async move { client.admin_channels_occupants(args).await })
            .await?;
        Ok((page.entries, page.next_cursor))
    }

    pub async fn admin_kick(
        &self,
        channel_jid: String,
        occupant_jid: String,
        reason: Option<String>,
    ) -> Result<(), String> {
        self.admin_call(move |client| async move {
            client
                .admin_channels_kick(channel_jid, occupant_jid, reason)
                .await
        })
        .await
        .map(|_| ())
    }

    pub async fn admin_users(
        &self,
        prefix: Option<String>,
        after: Option<String>,
    ) -> Result<PageOf<WaddleAdminUserEntry>, String> {
        let page = self
            .admin_call(move |client| async move {
                client
                    .admin_users_list(prefix, Some(PAGE_SIZE), after)
                    .await
            })
            .await?;
        Ok((page.entries, page.next_cursor))
    }
}

/// The text of a search box as a server prefix.
pub fn search_prefix(text: &str) -> Option<String> {
    optional(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn space() -> WaddleAdminSpaceEntry {
        WaddleAdminSpaceEntry {
            space_jid: "space@spaces.example.com".to_string(),
            space_node: "space".to_string(),
            name: "Penguins".to_string(),
            description: Some("Waddle on".to_string()),
            icon_url: None,
            channel_count: 2,
            member_count: 1,
        }
    }

    fn channel() -> WaddleAdminChannelListEntry {
        WaddleAdminChannelListEntry {
            channel_jid: "general@rooms.example.com".to_string(),
            name: "general".to_string(),
            topic: None,
            channel_type: "text".to_string(),
            is_public: true,
            members_only: false,
            occupant_count: 3,
            owner_count: 1,
            admin_count: 0,
            member_count: 2,
            outcast_count: 0,
        }
    }

    #[test]
    fn names_are_required_and_trimmed() {
        assert_eq!(required_name("  Ice  "), Ok("Ice".to_string()));
        assert!(required_name("   ").is_err());
    }

    #[test]
    fn creating_a_space_drops_blank_optionals() {
        let form = SpaceForm {
            name: " Floe ".to_string(),
            description: "  ".to_string(),
            icon_url: " https://x/i.png ".to_string(),
        };
        let args = form.create_args().unwrap();
        assert_eq!(args.name, "Floe");
        assert_eq!(args.description, None);
        assert_eq!(args.icon_url.as_deref(), Some("https://x/i.png"));
        assert!(SpaceForm::default().create_args().is_err());
    }

    #[test]
    fn updating_a_space_sends_only_what_changed() {
        let original = space();
        let mut form = SpaceForm::from_entry(&original);
        assert!(form.update_args(&original).unwrap().is_none());
        form.name = "Emperors".to_string();
        let args = form.update_args(&original).unwrap().unwrap();
        assert_eq!(args.name.as_deref(), Some("Emperors"));
        assert_eq!(args.description, None);
        assert_eq!(args.icon_url, None);
        assert_eq!(args.space_jid, original.space_jid);
        assert_eq!(args.space_node.as_deref(), Some("space"));
    }

    #[test]
    fn clearing_a_description_is_a_change() {
        let original = space();
        let mut form = SpaceForm::from_entry(&original);
        form.description.clear();
        let args = form.update_args(&original).unwrap().unwrap();
        assert_eq!(args.description.as_deref(), Some(""));
    }

    #[test]
    fn a_space_cannot_be_renamed_to_nothing() {
        let original = space();
        let mut form = SpaceForm::from_entry(&original);
        form.name = " ".to_string();
        assert!(form.update_args(&original).is_err());
    }

    #[test]
    fn creating_a_channel_targets_its_space() {
        let mut form = ChannelForm {
            name: "random".to_string(),
            ..ChannelForm::default()
        };
        form.members_only = true;
        let args = form.create_args(&space()).unwrap();
        assert_eq!(args.space_jid.as_deref(), Some("space@spaces.example.com"));
        assert_eq!(args.space_node.as_deref(), Some("space"));
        assert_eq!(args.channel_type.as_deref(), Some("text"));
        assert_eq!(args.is_public, Some(true));
        assert_eq!(args.members_only, Some(true));
        assert_eq!(args.topic, None);
    }

    #[test]
    fn updating_a_channel_sends_only_what_changed() {
        let original = channel();
        let mut form = ChannelForm::from_entry(&original);
        assert!(form.update_args(&original).unwrap().is_none());
        form.topic = "Hello".to_string();
        form.is_public = false;
        let args = form.update_args(&original).unwrap().unwrap();
        assert_eq!(args.topic.as_deref(), Some("Hello"));
        assert_eq!(args.is_public, Some(false));
        assert_eq!(args.name, None);
        assert_eq!(args.channel_type, None);
        assert_eq!(args.members_only, None);
    }

    #[test]
    fn channel_types_round_trip() {
        for (index, (wire, _)) in CHANNEL_TYPES.iter().enumerate() {
            let index = u32::try_from(index).unwrap();
            assert_eq!(channel_type_index(wire), index);
            assert_eq!(channel_type_wire(index), *wire);
        }
        assert_eq!(channel_type_wire(99), "text");
        assert_eq!(channel_type_label("group_dm"), "Group dm");
    }

    #[test]
    fn member_addresses_must_parse() {
        assert_eq!(
            member_jid(" Pingu@Example.com "),
            Ok("pingu@example.com".to_string())
        );
        assert!(member_jid("").is_err());
        assert!(member_jid("not a jid").is_err());
    }

    #[test]
    fn a_kick_names_the_bare_address() {
        assert_eq!(kick_target("a@x.org/phone"), "a@x.org");
        assert_eq!(kick_target("a@x.org"), "a@x.org");
    }

    #[test]
    fn my_address_matches_with_a_resource() {
        let me: BareJid = "me@example.com".parse().unwrap();
        assert!(is_me("me@example.com", &me));
        assert!(is_me("ME@example.com/laptop", &me));
        assert!(!is_me("you@example.com", &me));
    }

    #[test]
    fn roles_and_affiliations_index_both_ways() {
        for role in SPACE_ROLES {
            assert_eq!(space_role_at(space_role_index(role)), Some(role));
        }
        for affiliation in AFFILIATIONS {
            assert_eq!(
                affiliation_at(affiliation_index(affiliation)),
                Some(affiliation)
            );
        }
        assert_eq!(space_role_at(9), None);
        assert_eq!(affiliation_at(9), None);
        assert_eq!(role_names().len(), SPACE_ROLES.len());
        assert_eq!(affiliation_names().len(), AFFILIATIONS.len());
    }

    #[test]
    fn removing_from_a_space_confirms() {
        let ask = confirm_space_role("a@x", WaddleSpaceRole::Member, WaddleSpaceRole::None, false)
            .unwrap();
        assert_eq!(ask.action, "Remove");
        assert!(confirm_space_role(
            "a@x",
            WaddleSpaceRole::Member,
            WaddleSpaceRole::Admin,
            false
        )
        .is_none());
        assert!(
            confirm_space_role("a@x", WaddleSpaceRole::Admin, WaddleSpaceRole::Admin, false)
                .is_none()
        );
    }

    #[test]
    fn lowering_your_own_space_role_confirms() {
        assert!(
            confirm_space_role("me", WaddleSpaceRole::Owner, WaddleSpaceRole::Admin, true)
                .is_some()
        );
        // Raising yourself, or lowering someone else, is routine.
        assert!(
            confirm_space_role("me", WaddleSpaceRole::Admin, WaddleSpaceRole::Owner, true)
                .is_none()
        );
        assert!(
            confirm_space_role("a", WaddleSpaceRole::Owner, WaddleSpaceRole::Admin, false)
                .is_none()
        );
    }

    #[test]
    fn bans_and_removals_confirm_but_lifting_a_ban_does_not() {
        use WaddleMucAffiliation as A;
        assert_eq!(
            confirm_affiliation("a", A::Member, A::Outcast, false)
                .unwrap()
                .action,
            "Ban"
        );
        assert_eq!(
            confirm_affiliation("a", A::Member, A::None, false)
                .unwrap()
                .action,
            "Remove"
        );
        assert!(confirm_affiliation("a", A::Outcast, A::None, false).is_none());
        assert!(confirm_affiliation("a", A::None, A::Member, false).is_none());
        assert!(confirm_affiliation("a", A::Member, A::Member, false).is_none());
        assert!(confirm_affiliation("me", A::Owner, A::Member, true).is_some());
        assert!(confirm_affiliation("a", A::Owner, A::Member, false).is_none());
    }

    #[test]
    fn destructive_prompts_name_what_is_lost() {
        let prompt = delete_space_prompt(&space());
        assert_eq!(prompt.heading, "Delete Penguins?");
        assert!(prompt.body.contains("2 channels"));
        let prompt = delete_channel_prompt(&channel());
        assert_eq!(prompt.heading, "Delete #general?");
        assert_eq!(prompt.action, "Delete Channel");
    }

    #[test]
    fn summaries_pluralise() {
        let mut entry = space();
        entry.channel_count = 1;
        entry.member_count = 1;
        assert_eq!(space_summary(&entry), "Waddle on\n1 channel · 1 member");
        entry.description = None;
        assert_eq!(space_summary(&entry), "1 channel · 1 member");
        assert_eq!(channel_summary(&channel()), "Text · Public · 3 occupants");
        let mut private = channel();
        private.is_public = false;
        private.members_only = true;
        private.occupant_count = 1;
        assert_eq!(
            channel_summary(&private),
            "Text · Private · Members only · 1 occupant"
        );
    }

    #[test]
    fn users_show_their_display_name_or_address() {
        let user = |name: Option<&str>| WaddleAdminUserEntry {
            jid: "a@x".to_string(),
            display_name: name.map(str::to_string),
            has_owner_hat: false,
        };
        assert_eq!(user_title(&user(Some("Ann"))), "Ann");
        assert_eq!(user_title(&user(Some(" "))), "a@x");
        assert_eq!(user_title(&user(None)), "a@x");
    }

    #[test]
    fn errors_are_explained() {
        let stanza = |condition: &str, text: Option<&str>| WaddleError::Stanza {
            condition: condition.to_string(),
            text: text.map(str::to_string),
        };
        assert_eq!(
            describe_error(&stanza("forbidden", None)),
            "Only the community owner can do this."
        );
        assert_eq!(
            describe_error(&stanza("bad-request", Some("name too long"))),
            "name too long"
        );
        assert!(describe_error(&stanza("internal-server-error", None))
            .contains("internal-server-error"));
        assert!(describe_error(&WaddleError::Timeout).contains("too long"));
        assert!(describe_error(&WaddleError::NotConnected).contains("offline"));
    }

    #[test]
    fn paging_appends_and_stops() {
        let mut list: Paged<u32> = Paged::default();
        let first = list.begin(true).unwrap();
        assert_eq!(first.after, None);
        assert!(list.is_loading());
        assert!(list.begin(false).is_none());
        assert!(list.finish(&first, Ok((vec![1, 2], Some("c1".to_string())))));
        assert!(list.has_more());
        let second = list.begin(false).unwrap();
        assert_eq!(second.after.as_deref(), Some("c1"));
        assert!(list.finish(&second, Ok((vec![3], None))));
        assert_eq!(list.items, vec![1, 2, 3]);
        assert!(!list.has_more());
        assert!(list.begin(false).is_none());
    }

    #[test]
    fn a_restart_supersedes_a_read_in_flight() {
        let mut list: Paged<u32> = Paged::default();
        let stale = list.begin(true).unwrap();
        let fresh = list.begin(true).unwrap();
        assert!(!list.finish(&stale, Ok((vec![9], None))));
        assert!(list.items.is_empty());
        assert!(list.finish(&fresh, Ok((vec![1], None))));
        assert_eq!(list.items, vec![1]);
    }

    #[test]
    fn a_failed_page_keeps_rows_and_retries() {
        let mut list: Paged<u32> = Paged::default();
        let first = list.begin(true).unwrap();
        list.finish(&first, Ok((vec![1], Some("c1".to_string()))));
        let second = list.begin(false).unwrap();
        list.finish(&second, Err("offline".to_string()));
        assert_eq!(list.error(), Some("offline"));
        assert_eq!(list.items, vec![1]);
        let retry = list.begin(false).unwrap();
        assert_eq!(retry.after.as_deref(), Some("c1"));
        assert_eq!(list.error(), None);
    }

    #[test]
    fn a_failed_first_read_retries_from_the_start() {
        let mut list: Paged<u32> = Paged::default();
        let first = list.begin(true).unwrap();
        list.finish(&first, Err("offline".to_string()));
        assert!(!list.is_loaded());
        let retry = list.begin(false).unwrap();
        assert_eq!(retry.after, None);
    }

    #[test]
    fn a_repeated_cursor_ends_the_list() {
        let mut list: Paged<u32> = Paged::default();
        let first = list.begin(true).unwrap();
        list.finish(&first, Ok((vec![1], Some("c".to_string()))));
        let second = list.begin(false).unwrap();
        list.finish(&second, Ok((vec![2], Some("c".to_string()))));
        assert!(!list.has_more());
    }

    #[test]
    fn search_prefix_ignores_blank_text() {
        assert_eq!(search_prefix("  "), None);
        assert_eq!(search_prefix(" an "), Some("an".to_string()));
    }
}
