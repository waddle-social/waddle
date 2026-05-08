//! XEP-0004: Data Forms
//!
//! Provides a typed representation of XMPP data forms for use in workflows
//! such as service configuration, search, and reporting.
//!
//! ## Overview
//!
//! Data forms (`jabber:x:data`) are a lightweight protocol for exchanging
//! structured data between XMPP entities. A form-processing entity sends a
//! form of type `form`; the submitter replies with type `submit` or `cancel`.
//! A `result` type carries read-only tabular data.
//!
//! ## Field Types
//!
//! - `boolean` – true/false (wire values `1`/`0` or `true`/`false`)
//! - `fixed` – static label text (no `var` required)
//! - `hidden` – invisible to user, round-tripped unchanged
//! - `jid-multi` / `jid-single` – one or more JIDs
//! - `list-multi` / `list-single` – pick from options
//! - `text-multi` / `text-single` – free-form text
//! - `text-private` – password-style input
//!
//! ## Service Discovery
//!
//! Entities supporting data forms advertise `jabber:x:data` as a feature.

mod element;
mod types;

pub use element::{find_data_form, is_data_form, FromElement, IntoElement};
pub use types::{DataForm, DataFormError, Field, FieldOption, FieldType, FormType};

/// Namespace for XEP-0004 Data Forms.
pub const NS_DATA_FORMS: &str = "jabber:x:data";
#[cfg(test)]
mod tests;
