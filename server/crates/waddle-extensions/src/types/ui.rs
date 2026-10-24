//! Serializable UI blocks, forms, actions, and lists rendered by the host.

use super::*;

/// Named declarative view whose ordered blocks are rendered uniformly by the host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UiView {
    /// Identifier of this declarative view within the extension response.
    pub id: UiViewId,
    /// Human-readable title rendered by the host.
    pub title: Option<DisplayText>,
    /// UI blocks in their render order.
    pub blocks: Vec<UiBlock>,
}

impl UiView {
    pub(super) fn to_minidom(&self) -> Element {
        let mut builder = Element::builder("view", FRAMEWORK_NAMESPACE).attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            self.id.as_str(),
        );
        if let Some(title) = &self.title {
            builder = builder.attr(
                minidom::rxml::xml_ncname!("title").to_owned(),
                title.as_str(),
            );
        }
        for block in &self.blocks {
            builder = builder.append(block.to_minidom());
        }
        builder.build()
    }
}

/// Renderable text, image, action, form, or list within an extension view.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum UiBlock {
    /// Display styled text.
    Text(TextBlock),
    /// Display an immutable image artifact with alternative text.
    Image(ImageBlock),
    /// Display a control that invokes a referenced launch descriptor.
    Action(ActionBlock),
    /// Display or submit a typed form.
    Form(DataForm),
    /// Display an ordered list of items and optional launch controls.
    List(ListView),
}

impl UiBlock {
    fn to_minidom(&self) -> Element {
        match self {
            Self::Text(block) => block.to_minidom(),
            Self::Image(block) => block.to_minidom(),
            Self::Action(block) => block.to_minidom(),
            Self::Form(form) => form.to_minidom(),
            Self::List(list) => list.to_minidom(),
        }
    }
}

/// Display treatment applied to an extension text block.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum TextStyle {
    /// Ordinary body text.
    Body,
    /// Section heading text.
    Heading,
    /// Visually subdued secondary text.
    Muted,
    /// Monospaced code text.
    Code,
}

impl TextStyle {
    fn as_str(self) -> &'static str {
        match self {
            Self::Body => "body",
            Self::Heading => "heading",
            Self::Muted => "muted",
            Self::Code => "code",
        }
    }
}

/// Nonblank display text and the style requested by an extension.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TextBlock {
    /// Human-readable text associated with this value.
    pub text: DisplayText,
    /// Host display treatment requested for the text block.
    pub style: TextStyle,
}

impl TextBlock {
    fn to_minidom(&self) -> Element {
        Element::builder("text", FRAMEWORK_NAMESPACE)
            .attr(
                minidom::rxml::xml_ncname!("style").to_owned(),
                self.style.as_str(),
            )
            .append(self.text.as_str().to_string())
            .build()
    }
}

/// Immutable image artifact paired with accessible alternative text.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImageBlock {
    /// Immutable artifact referenced by this value.
    pub artifact: ArtifactReference,
    /// Nonblank alternative text describing the artifact image.
    pub alt: DisplayText,
}

impl ImageBlock {
    fn to_minidom(&self) -> Element {
        self.artifact
            .add_attrs(Element::builder("image", FRAMEWORK_NAMESPACE).attr(
                minidom::rxml::xml_ncname!("alt").to_owned(),
                self.alt.as_str(),
            ))
            .build()
    }
}

/// Button label referencing a launch descriptor from the same extension response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionBlock {
    /// Identifier of the launch associated with this value.
    pub launch_id: LaunchId,
    /// Human-readable label rendered by the host.
    pub label: DisplayText,
}

impl ActionBlock {
    fn to_minidom(&self) -> Element {
        Element::builder("action", FRAMEWORK_NAMESPACE)
            .attr(
                minidom::rxml::xml_ncname!("launch-id").to_owned(),
                self.launch_id.as_str(),
            )
            .attr(
                minidom::rxml::xml_ncname!("label").to_owned(),
                self.label.as_str(),
            )
            .build()
    }
}

/// Whether a form asks for input, submits it, cancels, or reports a result.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum DataFormType {
    /// Request user input using the supplied fields.
    Form,
    /// Submit field values to the command handler.
    Submit,
    /// Cancel the form interaction.
    Cancel,
    /// Report completed command output.
    Result,
}

impl DataFormType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Form => "form",
            Self::Submit => "submit",
            Self::Cancel => "cancel",
            Self::Result => "result",
        }
    }
}

/// Input and display kinds supported by the extension data-form vocabulary.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum FormFieldType {
    /// Single boolean choice.
    Boolean,
    /// Read-only text displayed without a submitted input.
    Fixed,
    /// Value retained and submitted without a visible input.
    Hidden,
    /// Multiple XMPP addresses.
    JidMulti,
    /// One XMPP address.
    JidSingle,
    /// Multiple choices from the offered options.
    ListMulti,
    /// One choice from the offered options.
    ListSingle,
    /// Multiline text input.
    TextMulti,
    /// Private text input rendered with obscured characters.
    TextPrivate,
    /// Single-line text input.
    TextSingle,
}

impl FormFieldType {
    fn as_str(self) -> &'static str {
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
}

/// One allowed form value and its optional user-visible label.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FormFieldOption {
    /// Human-readable label rendered by the host.
    pub label: Option<DisplayText>,
    /// Submitted form value selected when this option is chosen.
    pub value: DataFormValue,
}

/// Named form input, validation requirement, current values, and offered choices.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DataFormField {
    /// Machine-readable form field name serialized as the `var` attribute.
    pub name: UiActionId,
    /// Input or display kind determining how the form field is rendered.
    pub field_type: FormFieldType,
    /// Human-readable label rendered by the host.
    pub label: Option<DisplayText>,
    /// Whether a value must be supplied for this field.
    pub required: bool,
    /// Values associated with this field.
    pub values: Vec<DataFormValue>,
    /// Allowed values offered to the user.
    pub options: Vec<FormFieldOption>,
}

/// Form text retained verbatim, including empty strings and whitespace.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DataFormValue(String);

impl DataFormValue {
    /// Wrap form text verbatim; empty values are retained.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the stored text without allocating or changing whitespace.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume this wrapper and return its original owned text without copying.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

/// Typed extension form with a purpose, display instructions, and ordered fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DataForm {
    /// Purpose of this form: request input, submit, cancel, or report a result.
    pub form_type: DataFormType,
    /// Human-readable title rendered by the host.
    pub title: Option<DisplayText>,
    /// Instructions displayed to the user.
    pub instructions: Vec<DisplayText>,
    /// Fields carried by the data form or provider payload.
    pub fields: Vec<DataFormField>,
}

impl DataForm {
    fn to_minidom(&self) -> Element {
        let mut builder = Element::builder("form", FRAMEWORK_NAMESPACE).attr(
            minidom::rxml::xml_ncname!("type").to_owned(),
            self.form_type.as_str(),
        );
        if let Some(title) = &self.title {
            builder = builder.attr(
                minidom::rxml::xml_ncname!("title").to_owned(),
                title.as_str(),
            );
        }
        for instruction in &self.instructions {
            builder = builder.append(
                Element::builder("instructions", FRAMEWORK_NAMESPACE)
                    .append(instruction.as_str().to_string())
                    .build(),
            );
        }
        for field in &self.fields {
            let mut field_builder = Element::builder("field", FRAMEWORK_NAMESPACE)
                .attr(
                    minidom::rxml::xml_ncname!("var").to_owned(),
                    field.name.as_str(),
                )
                .attr(
                    minidom::rxml::xml_ncname!("type").to_owned(),
                    field.field_type.as_str(),
                );
            if let Some(label) = &field.label {
                field_builder = field_builder.attr(
                    minidom::rxml::xml_ncname!("label").to_owned(),
                    label.as_str(),
                );
            }
            if field.required {
                field_builder =
                    field_builder.append(Element::builder("required", FRAMEWORK_NAMESPACE).build());
            }
            for value in &field.values {
                field_builder = field_builder.append(
                    Element::builder("value", FRAMEWORK_NAMESPACE)
                        .append(value.as_str().to_string())
                        .build(),
                );
            }
            for option in &field.options {
                let mut option_builder = Element::builder("option", FRAMEWORK_NAMESPACE);
                if let Some(label) = &option.label {
                    option_builder = option_builder.attr(
                        minidom::rxml::xml_ncname!("label").to_owned(),
                        label.as_str(),
                    );
                }
                field_builder = field_builder.append(
                    option_builder
                        .append(
                            Element::builder("value", FRAMEWORK_NAMESPACE)
                                .append(option.value.as_str().to_string())
                                .build(),
                        )
                        .build(),
                );
            }
            builder = builder.append(field_builder.build());
        }
        builder.build()
    }
}

/// Named ordered list of display items with optional launch actions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListView {
    /// Identifier of this list within the declarative view.
    pub id: ListId,
    /// Human-readable title rendered by the host.
    pub title: Option<DisplayText>,
    /// Items carried in the publication or response.
    pub items: Vec<ListItem>,
}

impl ListView {
    fn to_minidom(&self) -> Element {
        let mut builder = Element::builder("list", FRAMEWORK_NAMESPACE).attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            self.id.as_str(),
        );
        if let Some(title) = &self.title {
            builder = builder.attr(
                minidom::rxml::xml_ncname!("title").to_owned(),
                title.as_str(),
            );
        }
        for item in &self.items {
            builder = builder.append(item.to_minidom());
        }
        builder.build()
    }
}

/// Labeled list entry with optional description, image, and launch action.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListItem {
    /// Identifier of this entry within its containing list.
    pub id: ListItemId,
    /// Human-readable label rendered by the host.
    pub label: DisplayText,
    /// Longer human-readable description.
    pub description: Option<DisplayText>,
    /// Optional immutable image artifact displayed with the list item.
    pub image: Option<ArtifactReference>,
    /// Identifier of the launch associated with this value.
    pub launch_id: Option<LaunchId>,
}

impl ListItem {
    fn to_minidom(&self) -> Element {
        let mut builder = Element::builder("item", FRAMEWORK_NAMESPACE)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                self.id.as_str(),
            )
            .attr(
                minidom::rxml::xml_ncname!("label").to_owned(),
                self.label.as_str(),
            );
        if let Some(description) = &self.description {
            builder = builder.attr(
                minidom::rxml::xml_ncname!("description").to_owned(),
                description.as_str(),
            );
        }
        if let Some(launch_id) = &self.launch_id {
            builder = builder.attr(
                minidom::rxml::xml_ncname!("launch-id").to_owned(),
                launch_id.as_str(),
            );
        }
        if let Some(image) = &self.image {
            builder = image.add_attrs(builder);
        }
        builder.build()
    }
}
