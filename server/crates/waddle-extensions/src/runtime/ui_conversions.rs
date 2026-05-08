use anyhow::Result;

use super::waddle::extension::types as wit_types;
use crate::types::{
    ActionBlock, ActionId, ArtifactReference, BodyRange, CommandNode, DataForm, DataFormField,
    DataFormType, DataFormValue, DisplayText, ExtensionPayload, FormFieldOption, FormFieldType,
    ImageBlock, LaunchContext, LaunchDescriptor, LaunchId, ListId, ListItem, ListItemId, ListView,
    MediaType, MessageSource, PayloadNamespace, PluginId, PubSubItemId, PubSubNode, PubSubPublish,
    RoomJid, StanzaId, TextBlock, TextStyle, Timestamp, UiActionId, UiBlock, UiView, UiViewId,
    WaddleId, XmlAttribute, XmlElement, XmlNode,
};

impl TryFrom<wit_types::MessageSource> for MessageSource {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::MessageSource) -> Result<Self> {
        Ok(Self {
            stanza_id: wit_newtype_to_domain!(value.stanza_id, StanzaId)?,
            body_range: value
                .body_range
                .map(|range| BodyRange::new(range.start, range.end))
                .transpose()?,
        })
    }
}

impl TryFrom<wit_types::LaunchDescriptor> for LaunchDescriptor {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::LaunchDescriptor) -> Result<Self> {
        Ok(Self {
            id: wit_newtype_to_domain!(value.id, LaunchId)?,
            plugin: wit_newtype_to_domain!(value.plugin, PluginId)?,
            action: wit_newtype_to_domain!(value.action, ActionId)?,
            command_node: wit_newtype_to_domain!(value.command_node, CommandNode)?,
            label: wit_newtype_to_domain!(value.label, DisplayText)?,
            context: value.context.try_into()?,
            payloads: value
                .payloads
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<Vec<_>>>()?,
            fallback: value.fallback.map(TryInto::try_into).transpose()?,
            expires_at: value
                .expires_at
                .map(|expires_at| wit_newtype_to_domain!(expires_at, Timestamp))
                .transpose()?,
            token: None,
        })
    }
}

impl TryFrom<wit_types::LaunchContext> for LaunchContext {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::LaunchContext) -> Result<Self> {
        Ok(Self {
            waddle_id: wit_newtype_to_domain!(value.waddle_id, WaddleId)?,
            room: value
                .room
                .map(|room| wit_newtype_to_domain!(room, RoomJid))
                .transpose()?,
            source_stanza_id: value
                .source_stanza_id
                .map(|stanza_id| wit_newtype_to_domain!(stanza_id, StanzaId))
                .transpose()?,
        })
    }
}

impl TryFrom<wit_types::UiView> for UiView {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::UiView) -> Result<Self> {
        Ok(Self {
            id: wit_newtype_to_domain!(value.id, UiViewId)?,
            title: value
                .title
                .map(|title| wit_newtype_to_domain!(title, DisplayText))
                .transpose()?,
            blocks: value
                .blocks
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<Vec<_>>>()?,
        })
    }
}

impl TryFrom<wit_types::UiBlock> for UiBlock {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::UiBlock) -> Result<Self> {
        Ok(match value {
            wit_types::UiBlock::Text(block) => Self::Text(block.try_into()?),
            wit_types::UiBlock::Image(block) => Self::Image(block.try_into()?),
            wit_types::UiBlock::Action(block) => Self::Action(block.try_into()?),
            wit_types::UiBlock::Form(form) => Self::Form(form.try_into()?),
            wit_types::UiBlock::ListBlock(list) => Self::List(list.try_into()?),
        })
    }
}

impl TryFrom<wit_types::TextBlock> for TextBlock {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::TextBlock) -> Result<Self> {
        Ok(Self {
            text: wit_newtype_to_domain!(value.text, DisplayText)?,
            style: value.style.into(),
        })
    }
}

impl From<wit_types::TextStyle> for TextStyle {
    fn from(value: wit_types::TextStyle) -> Self {
        match value {
            wit_types::TextStyle::Body => Self::Body,
            wit_types::TextStyle::Heading => Self::Heading,
            wit_types::TextStyle::Muted => Self::Muted,
            wit_types::TextStyle::Code => Self::Code,
        }
    }
}

impl TryFrom<wit_types::ImageBlock> for ImageBlock {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::ImageBlock) -> Result<Self> {
        Ok(Self {
            artifact: value.artifact.try_into()?,
            alt: wit_newtype_to_domain!(value.alt, DisplayText)?,
        })
    }
}

impl TryFrom<wit_types::ActionBlock> for ActionBlock {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::ActionBlock) -> Result<Self> {
        Ok(Self {
            launch_id: wit_newtype_to_domain!(value.launch_id, LaunchId)?,
            label: wit_newtype_to_domain!(value.label, DisplayText)?,
        })
    }
}

impl TryFrom<wit_types::DataForm> for DataForm {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::DataForm) -> Result<Self> {
        Ok(Self {
            form_type: value.form_type.into(),
            title: value
                .title
                .map(|title| wit_newtype_to_domain!(title, DisplayText))
                .transpose()?,
            instructions: value
                .instructions
                .into_iter()
                .map(|instruction| wit_newtype_to_domain!(instruction, DisplayText))
                .collect::<Result<Vec<_>>>()?,
            fields: value
                .fields
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<Vec<_>>>()?,
        })
    }
}

impl From<wit_types::DataFormType> for DataFormType {
    fn from(value: wit_types::DataFormType) -> Self {
        match value {
            wit_types::DataFormType::Form => Self::Form,
            wit_types::DataFormType::Submit => Self::Submit,
            wit_types::DataFormType::Cancel => Self::Cancel,
            wit_types::DataFormType::ResultForm => Self::Result,
        }
    }
}

impl TryFrom<wit_types::DataFormField> for DataFormField {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::DataFormField) -> Result<Self> {
        Ok(Self {
            name: wit_newtype_to_domain!(value.name, UiActionId)?,
            field_type: value.field_type.into(),
            label: value
                .label
                .map(|label| wit_newtype_to_domain!(label, DisplayText))
                .transpose()?,
            required: value.required,
            values: value
                .values
                .into_iter()
                .map(|value| DataFormValue::new(value.value))
                .collect(),
            options: value
                .options
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<Vec<_>>>()?,
        })
    }
}

impl From<wit_types::FormFieldType> for FormFieldType {
    fn from(value: wit_types::FormFieldType) -> Self {
        match value {
            wit_types::FormFieldType::Boolean => Self::Boolean,
            wit_types::FormFieldType::Fixed => Self::Fixed,
            wit_types::FormFieldType::Hidden => Self::Hidden,
            wit_types::FormFieldType::JidMulti => Self::JidMulti,
            wit_types::FormFieldType::JidSingle => Self::JidSingle,
            wit_types::FormFieldType::ListMulti => Self::ListMulti,
            wit_types::FormFieldType::ListSingle => Self::ListSingle,
            wit_types::FormFieldType::TextMulti => Self::TextMulti,
            wit_types::FormFieldType::TextPrivate => Self::TextPrivate,
            wit_types::FormFieldType::TextSingle => Self::TextSingle,
        }
    }
}

impl TryFrom<wit_types::FormFieldOption> for FormFieldOption {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::FormFieldOption) -> Result<Self> {
        Ok(Self {
            label: value
                .label
                .map(|label| wit_newtype_to_domain!(label, DisplayText))
                .transpose()?,
            value: DataFormValue::new(value.value.value),
        })
    }
}

impl TryFrom<wit_types::ListView> for ListView {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::ListView) -> Result<Self> {
        Ok(Self {
            id: wit_newtype_to_domain!(value.id, ListId)?,
            title: value
                .title
                .map(|title| wit_newtype_to_domain!(title, DisplayText))
                .transpose()?,
            items: value
                .items
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<Vec<_>>>()?,
        })
    }
}

impl TryFrom<wit_types::ListItem> for ListItem {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::ListItem) -> Result<Self> {
        Ok(Self {
            id: wit_newtype_to_domain!(value.id, ListItemId)?,
            label: wit_newtype_to_domain!(value.label, DisplayText)?,
            description: value
                .description
                .map(|description| wit_newtype_to_domain!(description, DisplayText))
                .transpose()?,
            image: value.image.map(TryInto::try_into).transpose()?,
            launch_id: value
                .launch_id
                .map(|launch_id| wit_newtype_to_domain!(launch_id, LaunchId))
                .transpose()?,
        })
    }
}

impl TryFrom<wit_types::ArtifactReference> for ArtifactReference {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::ArtifactReference) -> Result<Self> {
        ArtifactReference::new(
            value.uri.value,
            value.sha256.value,
            value
                .media_type
                .map(|media_type| wit_newtype_to_domain!(media_type, MediaType))
                .transpose()?,
        )
        .map_err(anyhow::Error::from)
    }
}

impl TryFrom<wit_types::ExtensionPayload> for ExtensionPayload {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::ExtensionPayload) -> Result<Self> {
        ExtensionPayload::new(
            wit_newtype_to_domain!(value.namespace, PayloadNamespace)?,
            xml_element_from_tokens(value.root, value.tokens)?,
        )
        .map_err(anyhow::Error::from)
    }
}

fn xml_element_from_tokens(
    root: wit_types::PayloadRoot,
    tokens: Vec<wit_types::XmlToken>,
) -> Result<XmlElement> {
    let expected_namespace = wit_newtype_to_domain!(root.namespace, PayloadNamespace)?;
    let expected_local_name = root.local_name;
    let mut stack: Vec<XmlElement> = Vec::new();
    let mut root_element = None;

    for token in tokens {
        match token {
            wit_types::XmlToken::StartElement(element) => {
                stack.push(element.try_into()?);
            }
            wit_types::XmlToken::Text(text) => {
                let Some(parent) = stack.last_mut() else {
                    return Err(anyhow::anyhow!("XML text token without open element"));
                };
                parent.children.push(XmlNode::Text(text));
            }
            wit_types::XmlToken::EndElement => {
                let Some(element) = stack.pop() else {
                    return Err(anyhow::anyhow!(
                        "XML end-element token without open element"
                    ));
                };
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(XmlNode::Element(element));
                } else if root_element.replace(element).is_some() {
                    return Err(anyhow::anyhow!("XML token stream has multiple roots"));
                }
            }
        }
    }

    if !stack.is_empty() {
        return Err(anyhow::anyhow!("XML token stream ended with open elements"));
    }
    let Some(element) = root_element else {
        return Err(anyhow::anyhow!("XML token stream has no root"));
    };
    if element.namespace != expected_namespace || element.local_name != expected_local_name {
        return Err(anyhow::anyhow!(
            "XML token stream root does not match declared root"
        ));
    }
    Ok(element)
}

impl TryFrom<wit_types::XmlElement> for XmlElement {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::XmlElement) -> Result<Self> {
        XmlElement::new(
            wit_newtype_to_domain!(value.namespace, PayloadNamespace)?,
            value.local_name,
            value
                .attributes
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<Vec<_>>>()?,
            Vec::new(),
        )
        .map_err(anyhow::Error::from)
    }
}

impl TryFrom<wit_types::XmlAttribute> for XmlAttribute {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::XmlAttribute) -> Result<Self> {
        Ok(Self {
            namespace: value
                .namespace
                .map(|namespace| wit_newtype_to_domain!(namespace, PayloadNamespace))
                .transpose()?,
            local_name: value.local_name,
            value: value.value,
        })
    }
}

impl TryFrom<wit_types::PubsubPublish> for PubSubPublish {
    type Error = anyhow::Error;

    fn try_from(value: wit_types::PubsubPublish) -> Result<Self> {
        Ok(Self {
            node: wit_newtype_to_domain!(value.node, PubSubNode)?,
            item_id: value
                .item_id
                .map(|item_id| wit_newtype_to_domain!(item_id, PubSubItemId))
                .transpose()?,
            payload: value.payload.try_into()?,
        })
    }
}
