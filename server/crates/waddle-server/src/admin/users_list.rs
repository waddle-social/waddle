//! XEP-0050 ad-hoc command `urn:xmpp:waddle:admin:users:list:0`.
//!
//! Owner-gated paginated list of registered users with optional
//! prefix search. Backs the V1 admin Users panel.
//!
//! ## Wire shape
//!
//! Request (args data form, `FORM_TYPE = urn:xmpp:waddle:admin:users:list:0`):
//!
//! - `prefix` (text-single, optional): case-insensitive prefix
//!   matched against `xmpp_localpart` and `display_name`.
//! - `page_size` (text-single, optional): page size; default 50,
//!   capped at 200.
//! - `after_cursor` (text-single, optional): seek-pagination cursor
//!   returned by a previous call.
//!
//! Result (`result` data form, `FORM_TYPE = urn:xmpp:waddle:admin:users:list:0`):
//!
//! - `reported` columns: `jid`, `display_name`, `has_owner_hat`.
//! - One `<item/>` per matching user.
//! - `next_cursor` (text-single, optional): present iff more results
//!   exist past the returned page. Opaque to the client.
//!
//! ## ACL
//!
//! The handler refuses any caller for whom
//! [`crate::admin::is_community_owner`] returns `false` with
//! `<forbidden/>` per XEP-0086 / RFC 6120 §8.3.3.4. There is no
//! "command session" for non-owners — the registry session is
//! discarded by the `CommandResult::Error` arm.

use std::sync::Arc;

use jid::{BareJid, Jid};
use waddle_xmpp::commands::{CommandContext, CommandResult};
use waddle_xmpp::xep::xep0004::{DataForm, Field, FieldType, FormType};
use waddle_xmpp::XmppError;

use crate::admin::is_community_owner;
use crate::db::actor::{DbActor, DbQuery};
use crate::db::{row_value, Value, ValueExt};
use crate::server::AppState;

/// XEP-0050 node identifier for the admin users-list command.
/// Mirrored from [`waddle_xmpp::admin::NS_ADMIN_USERS_LIST`] so the
/// disco feature advertisement and the registered handler key
/// remain in lockstep.
pub const NODE: &str = waddle_xmpp::admin::NS_ADMIN_USERS_LIST;
/// XEP-0004 `FORM_TYPE` value pinning the args/result data form.
/// We deliberately reuse the same URI string as [`NODE`]; this
/// matches the §3.3 "Discovering Commands" convention where a
/// command's node identifier and its FORM_TYPE coincide.
pub const FORM_TYPE: &str = waddle_xmpp::admin::NS_ADMIN_USERS_LIST;

/// Default page size when the caller omits `page_size`.
pub const DEFAULT_PAGE_SIZE: u32 = 50;
/// Hard cap on `page_size` regardless of the requested value.
pub const MAX_PAGE_SIZE: u32 = 200;

/// Parsed request arguments. Constructed via [`parse_args`] from the
/// submitted form; equality lets the parser tests assert exact
/// values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsersListArgs {
    /// Lowercased prefix used for matching. `None` = no prefix
    /// filter.
    pub prefix: Option<String>,
    /// Effective page size after applying the default + cap.
    pub page_size: u32,
    /// Opaque seek cursor from a previous response.
    pub after_cursor: Option<String>,
}

/// A typed row returned to the caller. Used to build the wire form
/// and exercised directly in tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsersListEntry {
    pub jid: BareJid,
    pub display_name: Option<String>,
    pub has_owner_hat: bool,
}

/// Typed page returned from [`run_users_list_query`]. Serialized to
/// a `result` data form by [`build_result_form`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsersListPage {
    pub entries: Vec<UsersListEntry>,
    pub next_cursor: Option<String>,
}

/// Register the admin users-list command on `registry`.
///
/// The registered handler captures `app_state` and `xmpp_domain`
/// (the user-bearing domain used to build full bare JIDs from
/// `xmpp_localpart` rows). Both are cheap-clone (Arc, String) so
/// the closure can clone them per dispatch.
pub async fn register(
    registry: &waddle_xmpp::commands::CommandRegistry,
    app_state: Arc<AppState>,
    xmpp_domain: String,
) {
    registry
        .register(NODE, "Admin · List users", move |ctx| {
            let app_state = Arc::clone(&app_state);
            let xmpp_domain = xmpp_domain.clone();
            async move { handle(ctx, app_state, xmpp_domain).await }
        })
        .await;
}

async fn handle(
    ctx: CommandContext,
    app_state: Arc<AppState>,
    xmpp_domain: String,
) -> CommandResult {
    let caller_bare = bare_from_jid(&ctx.from);
    if !caller_is_owner(&caller_bare, &app_state) {
        return CommandResult::Error(XmppError::forbidden(Some(
            "Admin commands require the community owner role".to_string(),
        )));
    }

    let args = match parse_args(ctx.command.form.as_ref()) {
        Ok(args) => args,
        Err(error) => {
            return CommandResult::Error(XmppError::bad_request(Some(error)));
        }
    };

    let page = match run_users_list_query(
        app_state.db_pool.global_actor(),
        &app_state.server_owner_jids,
        &xmpp_domain,
        &args,
    )
    .await
    {
        Ok(page) => page,
        Err(error) => return CommandResult::Error(error),
    };

    CommandResult::Completed {
        form: Some(build_result_form(&page)),
        notes: vec![],
    }
}

fn caller_is_owner(caller: &Option<BareJid>, app_state: &AppState) -> bool {
    caller
        .as_ref()
        .is_some_and(|jid| is_community_owner(app_state, jid))
}

fn bare_from_jid(jid: &Jid) -> Option<BareJid> {
    Some(jid.to_bare())
}

/// Parse the submitted args data form into a [`UsersListArgs`].
///
/// `form` is the `<x type='submit'>` carried in `<command/>`; missing
/// or unrelated forms (no `FORM_TYPE` hidden field matching
/// [`FORM_TYPE`], or a wrong form type) are treated as "no args" and
/// produce defaults. Unparseable `page_size` values are an error;
/// silently clamping would let a typo become an unbounded scan.
pub fn parse_args(form: Option<&DataForm>) -> Result<UsersListArgs, String> {
    let Some(form) = form else {
        return Ok(UsersListArgs::default());
    };
    if !matches!(form.form_type, FormType::Submit) {
        return Ok(UsersListArgs::default());
    }
    if form.get_form_type_value() != Some(FORM_TYPE) {
        // Foreign / mistyped FORM_TYPE — treat as "no args" rather
        // than rejecting; the client may have stripped a non-essential
        // hidden field.
    }

    let prefix = form
        .get_value("prefix")
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty());

    let page_size = match form.get_value("page_size") {
        Some(raw) if !raw.is_empty() => {
            let parsed: u32 = raw
                .parse()
                .map_err(|_| format!("page_size must be a positive integer, got '{}'", raw))?;
            parsed.clamp(1, MAX_PAGE_SIZE)
        }
        _ => DEFAULT_PAGE_SIZE,
    };

    let after_cursor = form
        .get_value("after_cursor")
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty());

    Ok(UsersListArgs {
        prefix,
        page_size,
        after_cursor,
    })
}

impl Default for UsersListArgs {
    fn default() -> Self {
        Self {
            prefix: None,
            page_size: DEFAULT_PAGE_SIZE,
            after_cursor: None,
        }
    }
}

/// Execute the seek-paginated query and assemble a [`UsersListPage`].
///
/// Ordering: `(xmpp_localpart ASC)`. Seek cursor: the encoded
/// localpart of the last entry on the previous page; the next page
/// starts strictly after it. The cursor is opaque to the client.
///
/// `owners` is the configured server-owner JID set; entries whose
/// JID matches an owner flip `has_owner_hat = true`. The flag is
/// derived per-row rather than via a SQL join so the helper has a
/// single canonical source of truth (the same set
/// [`is_community_owner`] consults).
pub async fn run_users_list_query(
    db: &kameo::actor::ActorRef<DbActor>,
    owners: &[BareJid],
    xmpp_domain: &str,
    args: &UsersListArgs,
) -> Result<UsersListPage, XmppError> {
    // Fetch `page_size + 1` so we can tell whether a next page
    // exists without a second query. The extra row is dropped from
    // the response and its predecessor's localpart becomes the
    // returned cursor.
    let limit = args.page_size as i64 + 1;
    let mut sql = String::from("SELECT xmpp_localpart, display_name FROM users WHERE 1 = 1");
    let mut params: Vec<Value> = Vec::new();

    if let Some(prefix) = args.prefix.as_ref() {
        // Match against the localpart OR display_name (case-insensitive).
        // `display_name` may be NULL, so we wrap it in COALESCE for the
        // comparison.
        sql.push_str(
            " AND (LOWER(xmpp_localpart) LIKE ? OR LOWER(COALESCE(display_name, '')) LIKE ?)",
        );
        let pattern = format!("{}%", prefix);
        params.push(pattern.clone().into());
        params.push(pattern.into());
    }

    if let Some(cursor) = args.after_cursor.as_ref() {
        sql.push_str(" AND xmpp_localpart > ?");
        params.push(cursor.clone().into());
    }

    sql.push_str(" ORDER BY xmpp_localpart ASC LIMIT ?");
    params.push(limit.into());

    let rows = db
        .ask(DbQuery { sql, params })
        .await
        .map_err(|e| XmppError::internal(format!("Failed to query users: {}", e)))?;

    let mut entries = Vec::with_capacity(args.page_size as usize);
    let mut next_cursor = None;
    let row_count = rows.len();
    for (idx, row) in rows.into_iter().enumerate() {
        if idx as u32 >= args.page_size {
            // Skip the extra row used purely as a "has more" probe;
            // the previous row's localpart already became the cursor
            // when we appended it to `entries`.
            break;
        }
        let localpart = row_value(&row, 0)
            .and_then(ValueExt::as_string)
            .map_err(|e| XmppError::internal(format!("Failed to decode localpart: {}", e)))?;
        let display_name = row_value(&row, 1)
            .and_then(ValueExt::as_optional_string)
            .map_err(|e| XmppError::internal(format!("Failed to decode display_name: {}", e)))?;
        let jid: BareJid = format!("{}@{}", localpart, xmpp_domain)
            .parse()
            .map_err(|e| XmppError::internal(format!("Failed to build JID: {}", e)))?;
        let has_owner_hat = owners.iter().any(|owner| owner == &jid);
        if (idx as u32 + 1) == args.page_size && row_count > args.page_size as usize {
            next_cursor = Some(localpart.clone());
        }
        entries.push(UsersListEntry {
            jid,
            display_name,
            has_owner_hat,
        });
    }

    Ok(UsersListPage {
        entries,
        next_cursor,
    })
}

/// Build the XEP-0004 `result` data form Waddle returns to the
/// caller. Each entry becomes one `<item/>` row.
pub fn build_result_form(page: &UsersListPage) -> DataForm {
    let mut form = DataForm::new(FormType::Result)
        .add_field(Field::form_type(FORM_TYPE))
        .add_reported(Field::new("jid", FieldType::JidSingle).with_label("JID"))
        .add_reported(Field::new("display_name", FieldType::TextSingle).with_label("Display name"))
        .add_reported(
            Field::new("has_owner_hat", FieldType::Boolean).with_label("Community owner"),
        );

    for entry in &page.entries {
        let row = vec![
            Field::new("jid", FieldType::JidSingle).with_value(entry.jid.to_string()),
            Field::new("display_name", FieldType::TextSingle)
                .with_value(entry.display_name.clone().unwrap_or_default()),
            Field::boolean("has_owner_hat", entry.has_owner_hat),
        ];
        form = form.add_item(row);
    }

    if let Some(cursor) = page.next_cursor.as_ref() {
        form = form.add_field(Field::text_single("next_cursor", cursor));
    }

    form
}

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_xmpp::xep::xep0004::{DataForm, Field, FormType};

    fn submit_form() -> DataForm {
        DataForm::new(FormType::Submit).add_field(Field::form_type(FORM_TYPE))
    }

    #[test]
    fn parse_args_no_form_returns_defaults() {
        let args = parse_args(None).expect("defaults");
        assert_eq!(args, UsersListArgs::default());
        assert_eq!(args.page_size, DEFAULT_PAGE_SIZE);
    }

    #[test]
    fn parse_args_lowercases_and_trims_prefix() {
        let form = submit_form().add_field(Field::text_single("prefix", "  Alice  "));
        let args = parse_args(Some(&form)).expect("ok");
        assert_eq!(args.prefix.as_deref(), Some("alice"));
    }

    #[test]
    fn parse_args_caps_page_size_at_max() {
        let form = submit_form().add_field(Field::text_single("page_size", "999"));
        let args = parse_args(Some(&form)).expect("ok");
        assert_eq!(args.page_size, MAX_PAGE_SIZE);
    }

    #[test]
    fn parse_args_rejects_non_numeric_page_size() {
        let form = submit_form().add_field(Field::text_single("page_size", "lots"));
        let err = parse_args(Some(&form)).expect_err("non-numeric");
        assert!(err.contains("page_size"), "{err}");
    }

    #[test]
    fn parse_args_carries_after_cursor() {
        let form = submit_form().add_field(Field::text_single("after_cursor", "charlie"));
        let args = parse_args(Some(&form)).expect("ok");
        assert_eq!(args.after_cursor.as_deref(), Some("charlie"));
    }

    #[test]
    fn build_result_form_includes_reported_columns_and_items() {
        let page = UsersListPage {
            entries: vec![UsersListEntry {
                jid: "alice@localhost".parse().expect("jid"),
                display_name: Some("Alice".to_string()),
                has_owner_hat: false,
            }],
            next_cursor: Some("alice".to_string()),
        };
        let form = build_result_form(&page);
        assert!(matches!(form.form_type, FormType::Result));
        assert_eq!(form.reported.len(), 3, "jid/display_name/has_owner_hat");
        assert_eq!(form.items.len(), 1);
        assert_eq!(form.get_value("next_cursor"), Some("alice"));
    }

    #[test]
    fn build_result_form_omits_next_cursor_when_absent() {
        let page = UsersListPage {
            entries: vec![],
            next_cursor: None,
        };
        let form = build_result_form(&page);
        assert!(form.field("next_cursor").is_none());
    }
}
