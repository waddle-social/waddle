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

use std::fmt;
use std::str::FromStr;

use minidom::Element;
use thiserror::Error;

/// Namespace for XEP-0004 Data Forms.
pub const NS_DATA_FORMS: &str = "jabber:x:data";

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors that can occur when parsing or validating a data form.
#[derive(Debug, Error)]
pub enum DataFormError {
    /// The form element has an invalid or missing `type` attribute.
    #[error("invalid form type: {0}")]
    InvalidFormType(String),

    /// A field element has an unrecognised `type` attribute.
    #[error("invalid field type: {0}")]
    InvalidFieldType(String),

    /// The element is not a valid `<x xmlns='jabber:x:data'>` form.
    #[error("element is not a data form")]
    NotADataForm,

    /// A required attribute or child element is missing.
    #[error("missing required element: {0}")]
    MissingElement(String),
}

// ---------------------------------------------------------------------------
// FormType
// ---------------------------------------------------------------------------

/// The type of a data form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FormType {
    /// A form to be filled in by the recipient.
    Form,
    /// A completed form submitted by the recipient.
    Submit,
    /// The recipient cancelled the form.
    Cancel,
    /// Read-only data, possibly tabular.
    Result,
}

impl fmt::Display for FormType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FormType {
    /// Return the wire string for this form type.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Form => "form",
            Self::Submit => "submit",
            Self::Cancel => "cancel",
            Self::Result => "result",
        }
    }
}

impl FromStr for FormType {
    type Err = DataFormError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "form" => Ok(Self::Form),
            "submit" => Ok(Self::Submit),
            "cancel" => Ok(Self::Cancel),
            "result" => Ok(Self::Result),
            other => Err(DataFormError::InvalidFormType(other.to_string())),
        }
    }
}

// ---------------------------------------------------------------------------
// FieldType
// ---------------------------------------------------------------------------

/// The type of a data form field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FieldType {
    /// A boolean value (`1`/`0` or `true`/`false`).
    Boolean,
    /// Static display text (no variable name required).
    Fixed,
    /// A field not shown to the user; round-tripped unchanged.
    Hidden,
    /// One or more JIDs.
    JidMulti,
    /// A single JID.
    JidSingle,
    /// Multiple selections from a list of options.
    ListMulti,
    /// A single selection from a list of options.
    ListSingle,
    /// Multiple lines of free-form text.
    TextMulti,
    /// A single line of text that should be obscured (password).
    TextPrivate,
    /// A single line of free-form text (default).
    #[default]
    TextSingle,
}

impl fmt::Display for FieldType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FieldType {
    /// Return the wire string for this field type.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Boolean => "boolean",
            Self::Fixed => "fixed",
            Self::Hidden => "hidden",
            Self::JidMulti => "jid-multi",
            Self::JidSingle => "jid-single",
            Self::ListMulti => "list-multi",
            Self::ListSingle => "list-single",
            Self::TextMulti => "text-multi",
            Self::TextPrivate => "text-private",
            Self::TextSingle => "text-single",
        }
    }

    /// Whether this field type supports multiple values.
    pub fn is_multi(&self) -> bool {
        matches!(self, Self::JidMulti | Self::ListMulti | Self::TextMulti)
    }
}

impl FromStr for FieldType {
    type Err = DataFormError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "boolean" => Ok(Self::Boolean),
            "fixed" => Ok(Self::Fixed),
            "hidden" => Ok(Self::Hidden),
            "jid-multi" => Ok(Self::JidMulti),
            "jid-single" => Ok(Self::JidSingle),
            "list-multi" => Ok(Self::ListMulti),
            "list-single" => Ok(Self::ListSingle),
            "text-multi" => Ok(Self::TextMulti),
            "text-private" => Ok(Self::TextPrivate),
            "text-single" => Ok(Self::TextSingle),
            other => Err(DataFormError::InvalidFieldType(other.to_string())),
        }
    }
}

// ---------------------------------------------------------------------------
// FieldOption
// ---------------------------------------------------------------------------

/// A selectable option within a `list-single` or `list-multi` field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldOption {
    /// Human-readable label for this option.
    pub label: Option<String>,
    /// The wire value submitted when this option is selected.
    pub value: String,
}

impl FieldOption {
    /// Create an option with only a value.
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            label: None,
            value: value.into(),
        }
    }

    /// Create an option with a label and value.
    pub fn with_label(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: Some(label.into()),
            value: value.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Field
// ---------------------------------------------------------------------------

/// A single field within a data form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// Variable name used to identify this field.
    pub var: Option<String>,
    /// The type of this field.
    pub field_type: FieldType,
    /// Human-readable label.
    pub label: Option<String>,
    /// Extended description / help text.
    pub desc: Option<String>,
    /// Whether the field is required.
    pub required: bool,
    /// Current value(s). Multi-valued fields may have more than one.
    pub values: Vec<String>,
    /// Available options (for `list-single` / `list-multi`).
    pub options: Vec<FieldOption>,
}

impl Field {
    /// Create a new field with a variable name and type.
    pub fn new(var: impl Into<String>, field_type: FieldType) -> Self {
        Self {
            var: Some(var.into()),
            field_type,
            label: None,
            desc: None,
            required: false,
            values: Vec::new(),
            options: Vec::new(),
        }
    }

    /// Create a hidden field (typically used for `FORM_TYPE`).
    pub fn hidden(var: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            var: Some(var.into()),
            field_type: FieldType::Hidden,
            label: None,
            desc: None,
            required: false,
            values: vec![value.into()],
            options: Vec::new(),
        }
    }

    /// Create a `FORM_TYPE` hidden field with the given namespace.
    pub fn form_type(ns: impl Into<String>) -> Self {
        Self::hidden("FORM_TYPE", ns)
    }

    /// Create a text-single field with a value.
    pub fn text_single(var: impl Into<String>, value: impl Into<String>) -> Self {
        let mut f = Self::new(var, FieldType::TextSingle);
        f.values.push(value.into());
        f
    }

    /// Create a boolean field with a value.
    pub fn boolean(var: impl Into<String>, value: bool) -> Self {
        let mut f = Self::new(var, FieldType::Boolean);
        f.values.push(if value {
            "1".to_string()
        } else {
            "0".to_string()
        });
        f
    }

    /// Create a fixed (display-only) field with text.
    pub fn fixed(text: impl Into<String>) -> Self {
        Self {
            var: None,
            field_type: FieldType::Fixed,
            label: None,
            desc: None,
            required: false,
            values: vec![text.into()],
            options: Vec::new(),
        }
    }

    /// Set the human-readable label.
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Set the description / help text.
    pub fn with_desc(mut self, desc: impl Into<String>) -> Self {
        self.desc = Some(desc.into());
        self
    }

    /// Mark the field as required.
    pub fn with_required(mut self) -> Self {
        self.required = true;
        self
    }

    /// Set a single value (replaces any existing values).
    pub fn with_value(mut self, value: impl Into<String>) -> Self {
        self.values = vec![value.into()];
        self
    }

    /// Add a value (for multi-valued fields).
    pub fn add_value(mut self, value: impl Into<String>) -> Self {
        self.values.push(value.into());
        self
    }

    /// Add an option (for list fields).
    pub fn add_option(mut self, option: FieldOption) -> Self {
        self.options.push(option);
        self
    }

    /// Get the first value, if any.
    pub fn value(&self) -> Option<&str> {
        self.values.first().map(|s| s.as_str())
    }

    /// Get the first value as a boolean.
    ///
    /// Recognises `1`, `true` as true; everything else as false.
    pub fn value_as_bool(&self) -> Option<bool> {
        self.value().map(|v| matches!(v, "1" | "true"))
    }
}

// ---------------------------------------------------------------------------
// DataForm
// ---------------------------------------------------------------------------

/// A complete XEP-0004 data form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataForm {
    /// The type of this form.
    pub form_type: FormType,
    /// Optional title displayed to the user.
    pub title: Option<String>,
    /// Optional instructions for the user. Multiple strings for multiple
    /// `<instructions>` elements.
    pub instructions: Vec<String>,
    /// The fields in this form.
    pub fields: Vec<Field>,
    /// Column definitions for tabular (`result`) forms.
    pub reported: Vec<Field>,
    /// Row data for tabular (`result`) forms. Each item is a list of fields.
    pub items: Vec<Vec<Field>>,
}

impl DataForm {
    /// Create a new data form of the given type.
    pub fn new(form_type: FormType) -> Self {
        Self {
            form_type,
            title: None,
            instructions: Vec::new(),
            fields: Vec::new(),
            reported: Vec::new(),
            items: Vec::new(),
        }
    }

    /// Set the title.
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Add an instruction line.
    pub fn add_instructions(mut self, text: impl Into<String>) -> Self {
        self.instructions.push(text.into());
        self
    }

    /// Add a field to the form.
    pub fn add_field(mut self, field: Field) -> Self {
        self.fields.push(field);
        self
    }

    /// Add a reported column definition.
    pub fn add_reported(mut self, field: Field) -> Self {
        self.reported.push(field);
        self
    }

    /// Add a row of data to the items.
    pub fn add_item(mut self, fields: Vec<Field>) -> Self {
        self.items.push(fields);
        self
    }

    /// Get the `FORM_TYPE` value, if present.
    pub fn get_form_type_value(&self) -> Option<&str> {
        self.fields
            .iter()
            .find(|f| f.var.as_deref() == Some("FORM_TYPE"))
            .and_then(|f| f.value())
    }

    /// Find a field by its `var` name.
    pub fn field(&self, var: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.var.as_deref() == Some(var))
    }

    /// Find a field by its `var` name (mutable).
    pub fn field_mut(&mut self, var: &str) -> Option<&mut Field> {
        self.fields
            .iter_mut()
            .find(|f| f.var.as_deref() == Some(var))
    }

    /// Get a field's first value by var name.
    pub fn get_value(&self, var: &str) -> Option<&str> {
        self.field(var).and_then(|f| f.value())
    }

    /// Get a field's values by var name.
    pub fn get_values(&self, var: &str) -> Option<&[String]> {
        self.field(var).map(|f| f.values.as_slice())
    }

    /// Get a field's value as a boolean.
    pub fn get_bool(&self, var: &str) -> Option<bool> {
        self.field(var).and_then(|f| f.value_as_bool())
    }
}

// ---------------------------------------------------------------------------
// Parsing from Element
// ---------------------------------------------------------------------------

/// Trait for types that can be parsed from a minidom `Element`.
pub trait FromElement: Sized {
    /// The error type returned on parse failure.
    type Error;

    /// Parse from a minidom `Element`.
    fn from_element(elem: &Element) -> Result<Self, Self::Error>;
}

/// Trait for types that can be serialized to a minidom `Element`.
pub trait IntoElement {
    /// Serialize into a minidom `Element`.
    #[allow(clippy::wrong_self_convention)]
    fn into_element(&self) -> Element;
}

impl FromElement for FieldOption {
    type Error = DataFormError;

    fn from_element(elem: &Element) -> Result<Self, Self::Error> {
        let label = elem.attr("label").map(|s| s.to_string());
        let value = elem
            .get_child("value", NS_DATA_FORMS)
            .map(|v| v.text())
            .unwrap_or_default();

        Ok(Self { label, value })
    }
}

impl IntoElement for FieldOption {
    fn into_element(&self) -> Element {
        let mut builder = Element::builder("option", NS_DATA_FORMS);

        if let Some(ref label) = self.label {
            builder = builder.attr("label", label.as_str());
        }

        builder
            .append(
                Element::builder("value", NS_DATA_FORMS)
                    .append(self.value.as_str())
                    .build(),
            )
            .build()
    }
}

impl FromElement for Field {
    type Error = DataFormError;

    fn from_element(elem: &Element) -> Result<Self, Self::Error> {
        let var = elem.attr("var").map(|s| s.to_string());

        let field_type = match elem.attr("type") {
            Some(t) => t.parse()?,
            None => FieldType::TextSingle,
        };

        let label = elem.attr("label").map(|s| s.to_string());

        let desc = elem
            .get_child("desc", NS_DATA_FORMS)
            .map(|d| d.text())
            .filter(|t| !t.is_empty());

        let required = elem.get_child("required", NS_DATA_FORMS).is_some();

        let values: Vec<String> = elem
            .children()
            .filter(|c| c.name() == "value" && c.ns() == NS_DATA_FORMS)
            .map(|v| v.text())
            .collect();

        let options: Vec<FieldOption> = elem
            .children()
            .filter(|c| c.name() == "option" && c.ns() == NS_DATA_FORMS)
            .filter_map(|o| FieldOption::from_element(o).ok())
            .collect();

        Ok(Self {
            var,
            field_type,
            label,
            desc,
            required,
            values,
            options,
        })
    }
}

impl IntoElement for Field {
    fn into_element(&self) -> Element {
        let mut builder =
            Element::builder("field", NS_DATA_FORMS).attr("type", self.field_type.as_str());

        if let Some(ref var) = self.var {
            builder = builder.attr("var", var.as_str());
        }

        if let Some(ref label) = self.label {
            builder = builder.attr("label", label.as_str());
        }

        if let Some(ref desc) = self.desc {
            builder = builder.append(
                Element::builder("desc", NS_DATA_FORMS)
                    .append(desc.as_str())
                    .build(),
            );
        }

        if self.required {
            builder = builder.append(Element::builder("required", NS_DATA_FORMS).build());
        }

        for value in &self.values {
            builder = builder.append(
                Element::builder("value", NS_DATA_FORMS)
                    .append(value.as_str())
                    .build(),
            );
        }

        for option in &self.options {
            builder = builder.append(option.into_element());
        }

        builder.build()
    }
}

impl FromElement for DataForm {
    type Error = DataFormError;

    fn from_element(elem: &Element) -> Result<Self, Self::Error> {
        if elem.name() != "x" || elem.ns() != NS_DATA_FORMS {
            return Err(DataFormError::NotADataForm);
        }

        let form_type: FormType = elem
            .attr("type")
            .ok_or_else(|| DataFormError::MissingElement("type attribute".to_string()))?
            .parse()?;

        let title = elem
            .get_child("title", NS_DATA_FORMS)
            .map(|t| t.text())
            .filter(|t| !t.is_empty());

        let instructions: Vec<String> = elem
            .children()
            .filter(|c| c.name() == "instructions" && c.ns() == NS_DATA_FORMS)
            .map(|i| i.text())
            .collect();

        let fields: Vec<Field> = elem
            .children()
            .filter(|c| c.name() == "field" && c.ns() == NS_DATA_FORMS)
            .filter_map(|f| Field::from_element(f).ok())
            .collect();

        let reported: Vec<Field> = elem
            .get_child("reported", NS_DATA_FORMS)
            .map(|r| {
                r.children()
                    .filter(|c| c.name() == "field" && c.ns() == NS_DATA_FORMS)
                    .filter_map(|f| Field::from_element(f).ok())
                    .collect()
            })
            .unwrap_or_default();

        let items: Vec<Vec<Field>> = elem
            .children()
            .filter(|c| c.name() == "item" && c.ns() == NS_DATA_FORMS)
            .map(|item| {
                item.children()
                    .filter(|c| c.name() == "field" && c.ns() == NS_DATA_FORMS)
                    .filter_map(|f| Field::from_element(f).ok())
                    .collect()
            })
            .collect();

        Ok(Self {
            form_type,
            title,
            instructions,
            fields,
            reported,
            items,
        })
    }
}

impl IntoElement for DataForm {
    fn into_element(&self) -> Element {
        let mut builder =
            Element::builder("x", NS_DATA_FORMS).attr("type", self.form_type.as_str());

        if let Some(ref title) = self.title {
            builder = builder.append(
                Element::builder("title", NS_DATA_FORMS)
                    .append(title.as_str())
                    .build(),
            );
        }

        for instruction in &self.instructions {
            builder = builder.append(
                Element::builder("instructions", NS_DATA_FORMS)
                    .append(instruction.as_str())
                    .build(),
            );
        }

        for field in &self.fields {
            builder = builder.append(field.into_element());
        }

        if !self.reported.is_empty() {
            let mut reported_builder = Element::builder("reported", NS_DATA_FORMS);
            for field in &self.reported {
                reported_builder = reported_builder.append(field.into_element());
            }
            builder = builder.append(reported_builder.build());
        }

        for item_fields in &self.items {
            let mut item_builder = Element::builder("item", NS_DATA_FORMS);
            for field in item_fields {
                item_builder = item_builder.append(field.into_element());
            }
            builder = builder.append(item_builder.build());
        }

        builder.build()
    }
}

// ---------------------------------------------------------------------------
// Convenience: Check if an Element is a data form
// ---------------------------------------------------------------------------

/// Check whether a minidom `Element` is a `<x xmlns='jabber:x:data'>` form.
pub fn is_data_form(elem: &Element) -> bool {
    elem.name() == "x" && elem.ns() == NS_DATA_FORMS
}

/// Try to extract a `DataForm` from a parent element's children.
pub fn find_data_form(parent: &Element) -> Option<Result<DataForm, DataFormError>> {
    parent
        .children()
        .find(|c| is_data_form(c))
        .map(DataForm::from_element)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- FormType ----

    #[test]
    fn test_form_type_round_trip() {
        for (s, expected) in [
            ("form", FormType::Form),
            ("submit", FormType::Submit),
            ("cancel", FormType::Cancel),
            ("result", FormType::Result),
        ] {
            let parsed: FormType = s.parse().expect(s);
            assert_eq!(parsed, expected);
            assert_eq!(parsed.as_str(), s);
            assert_eq!(parsed.to_string(), s);
        }
    }

    #[test]
    fn test_form_type_invalid() {
        assert!("invalid".parse::<FormType>().is_err());
    }

    // ---- FieldType ----

    #[test]
    fn test_field_type_round_trip() {
        for s in &[
            "boolean",
            "fixed",
            "hidden",
            "jid-multi",
            "jid-single",
            "list-multi",
            "list-single",
            "text-multi",
            "text-private",
            "text-single",
        ] {
            let parsed: FieldType = s.parse().expect(s);
            assert_eq!(parsed.as_str(), *s);
        }
    }

    #[test]
    fn test_field_type_is_multi() {
        assert!(FieldType::JidMulti.is_multi());
        assert!(FieldType::ListMulti.is_multi());
        assert!(FieldType::TextMulti.is_multi());
        assert!(!FieldType::TextSingle.is_multi());
        assert!(!FieldType::Boolean.is_multi());
        assert!(!FieldType::ListSingle.is_multi());
    }

    #[test]
    fn test_field_type_default() {
        assert_eq!(FieldType::default(), FieldType::TextSingle);
    }

    // ---- Field constructors ----

    #[test]
    fn test_field_hidden() {
        let f = Field::hidden("FORM_TYPE", "urn:example");
        assert_eq!(f.var.as_deref(), Some("FORM_TYPE"));
        assert_eq!(f.field_type, FieldType::Hidden);
        assert_eq!(f.value(), Some("urn:example"));
    }

    #[test]
    fn test_field_form_type() {
        let f = Field::form_type("urn:xmpp:mam:2");
        assert_eq!(f.var.as_deref(), Some("FORM_TYPE"));
        assert_eq!(f.value(), Some("urn:xmpp:mam:2"));
    }

    #[test]
    fn test_field_text_single() {
        let f = Field::text_single("name", "Alice");
        assert_eq!(f.field_type, FieldType::TextSingle);
        assert_eq!(f.value(), Some("Alice"));
    }

    #[test]
    fn test_field_boolean() {
        let t = Field::boolean("flag", true);
        assert_eq!(t.value(), Some("1"));
        assert_eq!(t.value_as_bool(), Some(true));

        let f = Field::boolean("flag", false);
        assert_eq!(f.value(), Some("0"));
        assert_eq!(f.value_as_bool(), Some(false));
    }

    #[test]
    fn test_field_fixed() {
        let f = Field::fixed("Please fill in the form.");
        assert!(f.var.is_none());
        assert_eq!(f.field_type, FieldType::Fixed);
        assert_eq!(f.value(), Some("Please fill in the form."));
    }

    #[test]
    fn test_field_builder_methods() {
        let f = Field::new("color", FieldType::ListSingle)
            .with_label("Favorite Color")
            .with_desc("Choose one")
            .with_required()
            .with_value("red")
            .add_option(FieldOption::with_label("Red", "red"))
            .add_option(FieldOption::with_label("Blue", "blue"));

        assert_eq!(f.label.as_deref(), Some("Favorite Color"));
        assert_eq!(f.desc.as_deref(), Some("Choose one"));
        assert!(f.required);
        assert_eq!(f.value(), Some("red"));
        assert_eq!(f.options.len(), 2);
    }

    #[test]
    fn test_field_add_value() {
        let f = Field::new("items", FieldType::ListMulti)
            .add_value("a")
            .add_value("b")
            .add_value("c");

        assert_eq!(f.values, vec!["a", "b", "c"]);
    }

    // ---- DataForm constructors ----

    #[test]
    fn test_data_form_builder() {
        let form = DataForm::new(FormType::Form)
            .with_title("Registration")
            .add_instructions("Fill in the fields below.")
            .add_field(Field::form_type("jabber:iq:register"))
            .add_field(
                Field::new("username", FieldType::TextSingle)
                    .with_label("Username")
                    .with_required(),
            )
            .add_field(
                Field::new("password", FieldType::TextPrivate)
                    .with_label("Password")
                    .with_required(),
            );

        assert_eq!(form.form_type, FormType::Form);
        assert_eq!(form.title.as_deref(), Some("Registration"));
        assert_eq!(form.instructions, vec!["Fill in the fields below."]);
        assert_eq!(form.fields.len(), 3);
        assert_eq!(form.get_form_type_value(), Some("jabber:iq:register"));
        assert_eq!(form.get_value("username"), None); // no value set
    }

    #[test]
    fn test_data_form_get_bool() {
        let form = DataForm::new(FormType::Submit)
            .add_field(Field::boolean("persistent", true))
            .add_field(Field::boolean("moderated", false));

        assert_eq!(form.get_bool("persistent"), Some(true));
        assert_eq!(form.get_bool("moderated"), Some(false));
        assert_eq!(form.get_bool("nonexistent"), None);
    }

    #[test]
    fn test_data_form_field_mut() {
        let mut form = DataForm::new(FormType::Form).add_field(Field::text_single("name", "old"));

        if let Some(f) = form.field_mut("name") {
            f.values = vec!["new".to_string()];
        }
        assert_eq!(form.get_value("name"), Some("new"));
    }

    // ---- Tabular (result) form ----

    #[test]
    fn test_data_form_tabular() {
        let form = DataForm::new(FormType::Result)
            .add_reported(Field::new("name", FieldType::TextSingle).with_label("Name"))
            .add_reported(Field::new("url", FieldType::TextSingle).with_label("URL"))
            .add_item(vec![
                Field::text_single("name", "Waddle"),
                Field::text_single("url", "https://waddle.social"),
            ])
            .add_item(vec![
                Field::text_single("name", "XMPP"),
                Field::text_single("url", "https://xmpp.org"),
            ]);

        assert_eq!(form.reported.len(), 2);
        assert_eq!(form.items.len(), 2);
        assert_eq!(form.items[0][0].value(), Some("Waddle"));
    }

    // ---- XML round-trip ----

    #[test]
    fn test_simple_form_round_trip() {
        let original = DataForm::new(FormType::Form)
            .with_title("Bot Configuration")
            .add_instructions("Fill out this form to configure your bot.")
            .add_field(Field::form_type("urn:example:bot"))
            .add_field(
                Field::new("botname", FieldType::TextSingle)
                    .with_label("Bot Name")
                    .with_required(),
            )
            .add_field(Field::boolean("public", true).with_label("Make public?"));

        let elem = original.into_element();
        let parsed = DataForm::from_element(&elem).expect("parse");

        assert_eq!(parsed.form_type, FormType::Form);
        assert_eq!(parsed.title, original.title);
        assert_eq!(parsed.instructions, original.instructions);
        assert_eq!(parsed.fields.len(), 3);
        assert_eq!(parsed.get_form_type_value(), Some("urn:example:bot"));
        assert_eq!(parsed.get_bool("public"), Some(true));
    }

    #[test]
    fn test_submit_form_round_trip() {
        let original = DataForm::new(FormType::Submit)
            .add_field(Field::form_type("urn:example"))
            .add_field(Field::text_single("name", "Alice"))
            .add_field(Field::boolean("agree", true));

        let elem = original.into_element();
        let parsed = DataForm::from_element(&elem).expect("parse");

        assert_eq!(parsed.form_type, FormType::Submit);
        assert_eq!(parsed.get_value("name"), Some("Alice"));
        assert_eq!(parsed.get_bool("agree"), Some(true));
    }

    #[test]
    fn test_list_field_round_trip() {
        let original = DataForm::new(FormType::Form).add_field(
            Field::new("color", FieldType::ListSingle)
                .with_label("Favorite Color")
                .with_value("red")
                .add_option(FieldOption::with_label("Red", "red"))
                .add_option(FieldOption::with_label("Green", "green"))
                .add_option(FieldOption::with_label("Blue", "blue")),
        );

        let elem = original.into_element();
        let parsed = DataForm::from_element(&elem).expect("parse");

        let field = parsed.field("color").expect("color field");
        assert_eq!(field.field_type, FieldType::ListSingle);
        assert_eq!(field.value(), Some("red"));
        assert_eq!(field.options.len(), 3);
        assert_eq!(field.options[0].label.as_deref(), Some("Red"));
        assert_eq!(field.options[0].value, "red");
    }

    #[test]
    fn test_multi_value_field_round_trip() {
        let original = DataForm::new(FormType::Submit).add_field(
            Field::new("features", FieldType::ListMulti)
                .add_value("chat")
                .add_value("video")
                .add_value("voice"),
        );

        let elem = original.into_element();
        let parsed = DataForm::from_element(&elem).expect("parse");

        let values = parsed.get_values("features").expect("features values");
        assert_eq!(values, &["chat", "video", "voice"]);
    }

    #[test]
    fn test_tabular_form_round_trip() {
        let original = DataForm::new(FormType::Result)
            .add_field(Field::form_type("jabber:iq:search"))
            .add_reported(Field::new("jid", FieldType::JidSingle).with_label("JID"))
            .add_reported(Field::new("name", FieldType::TextSingle).with_label("Name"))
            .add_item(vec![
                Field::text_single("jid", "alice@example.com"),
                Field::text_single("name", "Alice"),
            ])
            .add_item(vec![
                Field::text_single("jid", "bob@example.com"),
                Field::text_single("name", "Bob"),
            ]);

        let elem = original.into_element();
        let parsed = DataForm::from_element(&elem).expect("parse");

        assert_eq!(parsed.form_type, FormType::Result);
        assert_eq!(parsed.reported.len(), 2);
        assert_eq!(parsed.items.len(), 2);
        assert_eq!(parsed.items[0][0].value(), Some("alice@example.com"));
        assert_eq!(parsed.items[1][1].value(), Some("Bob"));
    }

    #[test]
    fn test_required_and_desc_round_trip() {
        let original = DataForm::new(FormType::Form).add_field(
            Field::new("email", FieldType::TextSingle)
                .with_label("Email")
                .with_desc("Your email address")
                .with_required(),
        );

        let elem = original.into_element();
        let parsed = DataForm::from_element(&elem).expect("parse");

        let field = parsed.field("email").expect("email field");
        assert!(field.required);
        assert_eq!(field.desc.as_deref(), Some("Your email address"));
        assert_eq!(field.label.as_deref(), Some("Email"));
    }

    #[test]
    fn test_cancel_form_round_trip() {
        let original = DataForm::new(FormType::Cancel);
        let elem = original.into_element();
        let parsed = DataForm::from_element(&elem).expect("parse");

        assert_eq!(parsed.form_type, FormType::Cancel);
        assert!(parsed.fields.is_empty());
    }

    // ---- Error cases ----

    #[test]
    fn test_not_a_data_form() {
        let elem = Element::builder("query", "jabber:iq:roster").build();
        assert!(matches!(
            DataForm::from_element(&elem),
            Err(DataFormError::NotADataForm)
        ));
    }

    #[test]
    fn test_missing_type_attribute() {
        let elem = Element::builder("x", NS_DATA_FORMS).build();
        assert!(matches!(
            DataForm::from_element(&elem),
            Err(DataFormError::MissingElement(_))
        ));
    }

    #[test]
    fn test_invalid_type_attribute() {
        let elem = Element::builder("x", NS_DATA_FORMS)
            .attr("type", "bogus")
            .build();
        assert!(matches!(
            DataForm::from_element(&elem),
            Err(DataFormError::InvalidFormType(_))
        ));
    }

    // ---- Utility functions ----

    #[test]
    fn test_is_data_form() {
        let good = Element::builder("x", NS_DATA_FORMS)
            .attr("type", "form")
            .build();
        assert!(is_data_form(&good));

        let bad_name = Element::builder("query", NS_DATA_FORMS).build();
        assert!(!is_data_form(&bad_name));

        let bad_ns = Element::builder("x", "jabber:iq:roster").build();
        assert!(!is_data_form(&bad_ns));
    }

    #[test]
    fn test_find_data_form() {
        let parent = Element::builder("query", "urn:example")
            .append(
                Element::builder("x", NS_DATA_FORMS)
                    .attr("type", "submit")
                    .append(Field::text_single("name", "test").into_element())
                    .build(),
            )
            .build();

        let form = find_data_form(&parent)
            .expect("should find form")
            .expect("should parse");

        assert_eq!(form.form_type, FormType::Submit);
        assert_eq!(form.get_value("name"), Some("test"));
    }

    #[test]
    fn test_find_data_form_none() {
        let parent = Element::builder("query", "urn:example").build();
        assert!(find_data_form(&parent).is_none());
    }

    // ---- Compatibility with existing inline forms ----

    #[test]
    fn test_parse_hand_built_server_info_form() {
        // Simulate the kind of form built in disco/info.rs
        let elem = Element::builder("x", NS_DATA_FORMS)
            .attr("type", "result")
            .append(
                Element::builder("field", NS_DATA_FORMS)
                    .attr("var", "FORM_TYPE")
                    .attr("type", "hidden")
                    .append(
                        Element::builder("value", NS_DATA_FORMS)
                            .append("urn:xmpp:serverinfo:0")
                            .build(),
                    )
                    .build(),
            )
            .append(
                Element::builder("field", NS_DATA_FORMS)
                    .attr("var", "abuse-addresses")
                    .append(
                        Element::builder("value", NS_DATA_FORMS)
                            .append("mailto:abuse@example.com")
                            .build(),
                    )
                    .build(),
            )
            .build();

        let form = DataForm::from_element(&elem).expect("parse");
        assert_eq!(form.form_type, FormType::Result);
        assert_eq!(form.get_form_type_value(), Some("urn:xmpp:serverinfo:0"));
        assert_eq!(
            form.get_value("abuse-addresses"),
            Some("mailto:abuse@example.com")
        );
    }

    #[test]
    fn test_parse_muc_config_form() {
        // Simulate the kind of form used in muc/owner.rs
        let elem = Element::builder("x", NS_DATA_FORMS)
            .attr("type", "submit")
            .append(
                Element::builder("field", NS_DATA_FORMS)
                    .attr("var", "FORM_TYPE")
                    .attr("type", "hidden")
                    .append(
                        Element::builder("value", NS_DATA_FORMS)
                            .append("http://jabber.org/protocol/muc#roomconfig")
                            .build(),
                    )
                    .build(),
            )
            .append(
                Element::builder("field", NS_DATA_FORMS)
                    .attr("var", "muc#roomconfig_roomname")
                    .append(
                        Element::builder("value", NS_DATA_FORMS)
                            .append("Test Room")
                            .build(),
                    )
                    .build(),
            )
            .append(
                Element::builder("field", NS_DATA_FORMS)
                    .attr("var", "muc#roomconfig_persistentroom")
                    .append(Element::builder("value", NS_DATA_FORMS).append("1").build())
                    .build(),
            )
            .build();

        let form = DataForm::from_element(&elem).expect("parse");
        assert_eq!(form.form_type, FormType::Submit);
        assert_eq!(form.get_value("muc#roomconfig_roomname"), Some("Test Room"));
        assert_eq!(form.get_bool("muc#roomconfig_persistentroom"), Some(true));
    }

    // ---- Boolean value parsing edge cases ----

    #[test]
    fn test_boolean_value_variants() {
        // "true" and "1" should both be true
        for v in &["1", "true"] {
            let f = Field::new("test", FieldType::Boolean).with_value(*v);
            assert_eq!(f.value_as_bool(), Some(true), "expected true for {v}");
        }
        // "0", "false", and anything else should be false
        for v in &["0", "false", "maybe", ""] {
            let f = Field::new("test", FieldType::Boolean).with_value(*v);
            assert_eq!(f.value_as_bool(), Some(false), "expected false for {v}");
        }
    }

    #[test]
    fn test_empty_field_value_as_bool() {
        let f = Field::new("test", FieldType::Boolean);
        assert_eq!(f.value_as_bool(), None);
    }
}
