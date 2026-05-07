use std::fmt;
use std::str::FromStr;

use thiserror::Error;

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
