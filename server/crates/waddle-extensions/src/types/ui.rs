use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UiView {
    pub id: UiViewId,
    pub title: Option<DisplayText>,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum UiBlock {
    Text(TextBlock),
    Image(ImageBlock),
    Action(ActionBlock),
    Form(DataForm),
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum TextStyle {
    Body,
    Heading,
    Muted,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TextBlock {
    pub text: DisplayText,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImageBlock {
    pub artifact: ArtifactReference,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionBlock {
    pub launch_id: LaunchId,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum DataFormType {
    Form,
    Submit,
    Cancel,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum FormFieldType {
    Boolean,
    Fixed,
    Hidden,
    JidMulti,
    JidSingle,
    ListMulti,
    ListSingle,
    TextMulti,
    TextPrivate,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FormFieldOption {
    pub label: Option<DisplayText>,
    pub value: DataFormValue,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DataFormField {
    pub name: UiActionId,
    pub field_type: FormFieldType,
    pub label: Option<DisplayText>,
    pub required: bool,
    pub values: Vec<DataFormValue>,
    pub options: Vec<FormFieldOption>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DataFormValue(String);

impl DataFormValue {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DataForm {
    pub form_type: DataFormType,
    pub title: Option<DisplayText>,
    pub instructions: Vec<DisplayText>,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListView {
    pub id: ListId,
    pub title: Option<DisplayText>,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListItem {
    pub id: ListItemId,
    pub label: DisplayText,
    pub description: Option<DisplayText>,
    pub image: Option<ArtifactReference>,
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
