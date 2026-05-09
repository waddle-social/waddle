use tracing::{debug, warn};
use waddle_xmpp::{roster, XmppError};

use crate::db::roster::{
    DatabaseRosterStorage, RosterItemRow, RosterRowChange, RosterRowMutationKind,
    RosterStorageError,
};
use crate::db::Database;

pub(crate) async fn get_roster(
    db: Database,
    user_jid: &jid::BareJid,
) -> Result<Vec<roster::RosterItem>, XmppError> {
    debug!(jid = %user_jid, "Getting roster");

    let storage = DatabaseRosterStorage::new(db);
    let rows = storage.get_roster(user_jid).await.map_err(|e| {
        warn!(jid = %user_jid, error = %e, "Failed to get roster");
        XmppError::internal(format!("Database error: {}", e))
    })?;

    let items: Result<Vec<_>, _> = rows.iter().map(row_to_roster_item).collect();
    items.map_err(|e| {
        warn!(jid = %user_jid, error = %e, "Failed to convert roster items");
        XmppError::internal(format!("Roster conversion error: {}", e))
    })
}

pub(crate) async fn get_roster_item(
    db: Database,
    user_jid: &jid::BareJid,
    contact_jid: &jid::BareJid,
) -> Result<Option<roster::RosterItem>, XmppError> {
    debug!(user = %user_jid, contact = %contact_jid, "Getting roster item");

    let storage = DatabaseRosterStorage::new(db);
    let row = storage.get_roster_item(user_jid, contact_jid).await.map_err(|e| {
        warn!(user = %user_jid, contact = %contact_jid, error = %e, "Failed to get roster item");
        XmppError::internal(format!("Database error: {}", e))
    })?;

    match row {
        Some(row) => row_to_roster_item(&row).map(Some).map_err(|e| {
            warn!(user = %user_jid, contact = %contact_jid, error = %e, "Failed to convert roster item");
            XmppError::internal(format!("Roster conversion error: {}", e))
        }),
        None => Ok(None),
    }
}

pub(crate) async fn set_roster_item(
    db: Database,
    user_jid: &jid::BareJid,
    item: &roster::RosterItem,
) -> Result<roster::RosterSetResult, XmppError> {
    debug!(user = %user_jid, contact = %item.jid, "Setting roster item");

    let storage = DatabaseRosterStorage::new(db);
    let (mutation, _lock) = storage
        .apply_roster_change(user_jid, RosterRowChange::Upsert(roster_item_to_row(item)))
        .await
        .map_err(|e| {
            warn!(user = %user_jid, contact = %item.jid, error = %e, "Failed to set roster item");
            XmppError::internal(format!("Database error: {}", e))
        })?;

    Ok(match mutation.kind {
        RosterRowMutationKind::Added(_) => roster::RosterSetResult::Added(item.clone()),
        RosterRowMutationKind::Updated(_) => roster::RosterSetResult::Updated(item.clone()),
        RosterRowMutationKind::Removed(_) => unreachable!("Upsert never reports Removed"),
    })
}

pub(crate) async fn remove_roster_item(
    db: Database,
    user_jid: &jid::BareJid,
    contact_jid: &jid::BareJid,
) -> Result<bool, XmppError> {
    debug!(user = %user_jid, contact = %contact_jid, "Removing roster item");

    let storage = DatabaseRosterStorage::new(db);
    match storage
        .apply_roster_change(user_jid, RosterRowChange::Remove(contact_jid.clone()))
        .await
    {
        Ok(_) => Ok(true),
        Err(RosterStorageError::ItemNotFound) => Ok(false),
        Err(e) => {
            warn!(user = %user_jid, contact = %contact_jid, error = %e, "Failed to remove roster item");
            Err(XmppError::internal(format!("Database error: {}", e)))
        }
    }
}

pub(crate) async fn get_roster_version(
    db: Database,
    user_jid: &jid::BareJid,
) -> Result<Option<String>, XmppError> {
    debug!(jid = %user_jid, "Getting roster version");

    let storage = DatabaseRosterStorage::new(db);
    storage.get_roster_version(user_jid).await.map_err(|e| {
        warn!(jid = %user_jid, error = %e, "Failed to get roster version");
        XmppError::internal(format!("Database error: {}", e))
    })
}

pub(crate) async fn update_roster_subscription(
    db: Database,
    user_jid: &jid::BareJid,
    contact_jid: &jid::BareJid,
    subscription: roster::Subscription,
    ask: Option<roster::AskType>,
) -> Result<roster::RosterItem, XmppError> {
    debug!(
        user = %user_jid,
        contact = %contact_jid,
        subscription = %subscription,
        ask = ?ask,
        "Updating roster subscription"
    );

    let storage = DatabaseRosterStorage::new(db);
    let subscription_str = subscription.as_str();
    let ask_str = ask.map(|a| a.as_str());

    let (mutation, _lock) = storage
        .apply_subscription_update(user_jid, contact_jid, subscription_str, ask_str)
        .await
        .map_err(|e| {
            warn!(
                user = %user_jid,
                contact = %contact_jid,
                error = %e,
                "Failed to update roster subscription"
            );
            XmppError::internal(format!("Database error: {}", e))
        })?;

    let row = match mutation.kind {
        RosterRowMutationKind::Added(row) | RosterRowMutationKind::Updated(row) => row,
        RosterRowMutationKind::Removed(_) => {
            unreachable!("apply_subscription_update never reports Removed")
        }
    };

    row_to_roster_item(&row).map_err(|e| {
        warn!(user = %user_jid, contact = %contact_jid, error = %e, "Failed to convert roster item");
        XmppError::internal(format!("Roster conversion error: {}", e))
    })
}

pub(crate) async fn get_presence_subscribers(
    db: Database,
    user_jid: &jid::BareJid,
) -> Result<Vec<jid::BareJid>, XmppError> {
    debug!(jid = %user_jid, "Getting presence subscribers");

    let storage = DatabaseRosterStorage::new(db);
    storage.get_presence_subscribers(user_jid).await.map_err(|e| {
        warn!(jid = %user_jid, error = %e, "Failed to get presence subscribers");
        XmppError::internal(format!("Database error: {}", e))
    })
}

pub(crate) async fn get_presence_subscriptions(
    db: Database,
    user_jid: &jid::BareJid,
) -> Result<Vec<jid::BareJid>, XmppError> {
    debug!(jid = %user_jid, "Getting presence subscriptions");

    let storage = DatabaseRosterStorage::new(db);
    let jid_strings = storage
        .get_presence_subscriptions(user_jid)
        .await
        .map_err(|e| {
            warn!(jid = %user_jid, error = %e, "Failed to get presence subscriptions");
            XmppError::internal(format!("Database error: {}", e))
        })?;

    parse_bare_jids(&jid_strings, user_jid, "presence subscription")
}

fn parse_bare_jids(
    values: &[String],
    user_jid: &jid::BareJid,
    label: &'static str,
) -> Result<Vec<jid::BareJid>, XmppError> {
    let jids: Result<Vec<_>, _> = values.iter().map(|s| s.parse::<jid::BareJid>()).collect();
    jids.map_err(|e| {
        warn!(jid = %user_jid, error = ?e, "Failed to parse {label} JIDs");
        XmppError::internal(format!("JID parse error: {:?}", e))
    })
}

fn row_to_roster_item(row: &RosterItemRow) -> Result<roster::RosterItem, String> {
    let jid: jid::BareJid = row
        .contact_jid
        .parse()
        .map_err(|e| format!("Invalid JID '{}': {:?}", row.contact_jid, e))?;

    let subscription = row
        .subscription
        .parse::<roster::Subscription>()
        .map_err(|e| format!("Invalid subscription '{}': {}", row.subscription, e))?;

    let ask = match &row.ask {
        Some(a) => Some(
            a.parse::<roster::AskType>()
                .map_err(|e| format!("Invalid ask '{}': {}", a, e))?,
        ),
        None => None,
    };

    Ok(roster::RosterItem {
        jid,
        name: row.name.clone(),
        subscription,
        ask,
        approved: row.approved,
        groups: row.groups.clone(),
    })
}

fn roster_item_to_row(item: &roster::RosterItem) -> RosterItemRow {
    RosterItemRow {
        contact_jid: item.jid.to_string(),
        name: item.name.clone(),
        subscription: item.subscription.as_str().to_string(),
        ask: item.ask.map(|a| a.as_str().to_string()),
        approved: item.approved,
        groups: item.groups.clone(),
    }
}
