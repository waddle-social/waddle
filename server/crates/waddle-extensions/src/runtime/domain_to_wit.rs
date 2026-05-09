use super::waddle::extension::types as wit_types;
use crate::types::{
    ActionId, CommandAction, CommandInvocation, CommandNode, CommandSessionId, DataForm,
    DataFormField, DataFormType, DataFormValue, DisplayText, EnrichmentId, ExtensionEvent,
    ExtensionPayload, FormFieldOption, FormFieldType, FormFieldValue, FullJidValue, LaunchContext,
    LaunchId, LaunchInvocation, LinkTarget, ListId, ListItemId, MediaType, MessageContext,
    MessageHook, PayloadNamespace, PayloadRoot, PluginId, ProviderDeliveryId, ProviderEventType,
    ProviderField, ProviderFieldName, ProviderFieldNumber, ProviderFieldText, ProviderFieldValue,
    ProviderId, ProviderPayload, ProviderWebhook, PubSubItemId, PubSubNode, ReplyTarget, RoomJid,
    Sha256Digest, StanzaId, ThreadId, Timestamp, UiActionId, UiViewId, Url, WaddleId, XmlAttribute,
    XmlElement, XmlNode,
};

impl From<ExtensionEvent> for wit_types::ExtensionEvent {
    fn from(value: ExtensionEvent) -> Self {
        match value {
            ExtensionEvent::MessageHook(event) => Self::MessageHook(event.into()),
            ExtensionEvent::Command(event) => Self::Command(event.into()),
            ExtensionEvent::Launch(event) => Self::Launch(event.into()),
            ExtensionEvent::ProviderWebhook(event) => Self::ProviderWebhook(event.into()),
        }
    }
}

impl From<MessageHook> for wit_types::MessageHook {
    fn from(value: MessageHook) -> Self {
        Self {
            context: value.context.into(),
            body: value.body.into(),
            links: value.links.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<MessageContext> for wit_types::MessageContext {
    fn from(value: MessageContext) -> Self {
        Self {
            waddle_id: value.waddle_id.into(),
            stanza_id: value.stanza_id.map(Into::into),
            room: value.room.map(Into::into),
            sender: value.sender.map(Into::into),
            thread_id: value.thread_id.map(Into::into),
            reply_to: value.reply_to.map(Into::into),
        }
    }
}

impl From<ReplyTarget> for wit_types::ReplyTarget {
    fn from(value: ReplyTarget) -> Self {
        Self {
            id: value.id.into(),
            to: value.to.map(Into::into),
        }
    }
}

impl From<LinkTarget> for wit_types::LinkTarget {
    fn from(value: LinkTarget) -> Self {
        Self {
            url: value.url.into(),
            range: wit_types::BodyRange {
                start: value.range.start,
                end: value.range.end,
            },
        }
    }
}

impl From<CommandInvocation> for wit_types::CommandInvocation {
    fn from(value: CommandInvocation) -> Self {
        Self {
            waddle_id: value.waddle_id.into(),
            room: value.room.map(Into::into),
            requester: value.requester.into(),
            command_node: value.command_node.into(),
            session_id: value.session_id.map(Into::into),
            action: value.action.map(Into::into),
            form: value.form.map(Into::into),
            fields: value.fields.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<LaunchInvocation> for wit_types::LaunchInvocation {
    fn from(value: LaunchInvocation) -> Self {
        Self {
            context: value.context.into(),
            requester: value.requester.into(),
            launch_id: value.launch_id.into(),
            session_id: value.session_id.map(Into::into),
            action: value.action.map(Into::into),
            form: value.form.map(Into::into),
            fields: value.fields.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<ProviderWebhook> for wit_types::ProviderWebhook {
    fn from(value: ProviderWebhook) -> Self {
        Self {
            waddle_id: value.waddle_id.into(),
            provider: value.provider.into(),
            event_type: value.event_type.into(),
            delivery_id: value.delivery_id.into(),
            payload: value.payload.into(),
        }
    }
}

impl From<ProviderPayload> for wit_types::ProviderPayload {
    fn from(value: ProviderPayload) -> Self {
        Self {
            fields: value.fields.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<ProviderField> for wit_types::ProviderField {
    fn from(value: ProviderField) -> Self {
        Self {
            path: value.path.into_iter().map(Into::into).collect(),
            value: value.value.into(),
        }
    }
}

impl From<ProviderFieldValue> for wit_types::ProviderFieldValue {
    fn from(value: ProviderFieldValue) -> Self {
        match value {
            ProviderFieldValue::Null => Self::Null,
            ProviderFieldValue::Boolean(value) => Self::Boolean(value),
            ProviderFieldValue::Number(value) => Self::Number(value.into()),
            ProviderFieldValue::Text(value) => Self::Text(value.into()),
        }
    }
}

impl From<FormFieldValue> for wit_types::FormFieldValue {
    fn from(value: FormFieldValue) -> Self {
        Self {
            name: value.name.into(),
            values: value.values.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<CommandAction> for wit_types::CommandAction {
    fn from(value: CommandAction) -> Self {
        match value {
            CommandAction::Execute => Self::Execute,
            CommandAction::Next => Self::Next,
            CommandAction::Prev => Self::Prev,
            CommandAction::Complete => Self::Complete,
            CommandAction::Cancel => Self::Cancel,
        }
    }
}

impl From<LaunchContext> for wit_types::LaunchContext {
    fn from(value: LaunchContext) -> Self {
        Self {
            waddle_id: value.waddle_id.into(),
            room: value.room.map(Into::into),
            source_stanza_id: value.source_stanza_id.map(Into::into),
        }
    }
}

impl From<DataForm> for wit_types::DataForm {
    fn from(value: DataForm) -> Self {
        Self {
            form_type: value.form_type.into(),
            title: value.title.map(Into::into),
            instructions: value.instructions.into_iter().map(Into::into).collect(),
            fields: value.fields.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<DataFormType> for wit_types::DataFormType {
    fn from(value: DataFormType) -> Self {
        match value {
            DataFormType::Form => Self::Form,
            DataFormType::Submit => Self::Submit,
            DataFormType::Cancel => Self::Cancel,
            DataFormType::Result => Self::ResultForm,
        }
    }
}

impl From<FormFieldType> for wit_types::FormFieldType {
    fn from(value: FormFieldType) -> Self {
        match value {
            FormFieldType::Boolean => Self::Boolean,
            FormFieldType::Fixed => Self::Fixed,
            FormFieldType::Hidden => Self::Hidden,
            FormFieldType::JidMulti => Self::JidMulti,
            FormFieldType::JidSingle => Self::JidSingle,
            FormFieldType::ListMulti => Self::ListMulti,
            FormFieldType::ListSingle => Self::ListSingle,
            FormFieldType::TextMulti => Self::TextMulti,
            FormFieldType::TextPrivate => Self::TextPrivate,
            FormFieldType::TextSingle => Self::TextSingle,
        }
    }
}

impl From<DataFormField> for wit_types::DataFormField {
    fn from(value: DataFormField) -> Self {
        Self {
            name: value.name.into(),
            field_type: value.field_type.into(),
            label: value.label.map(Into::into),
            required: value.required,
            values: value.values.into_iter().map(Into::into).collect(),
            options: value.options.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<FormFieldOption> for wit_types::FormFieldOption {
    fn from(value: FormFieldOption) -> Self {
        Self {
            label: value.label.map(Into::into),
            value: value.value.into(),
        }
    }
}

impl From<DataFormValue> for wit_types::DataFormValue {
    fn from(value: DataFormValue) -> Self {
        Self {
            value: value.into_string(),
        }
    }
}

impl From<ExtensionPayload> for wit_types::ExtensionPayload {
    fn from(value: ExtensionPayload) -> Self {
        let mut tokens = Vec::new();
        push_xml_tokens(&value.root, &mut tokens);
        Self {
            namespace: value.namespace.into(),
            root: PayloadRoot {
                namespace: value.root.namespace.clone(),
                local_name: value.root.local_name.clone(),
            }
            .into(),
            tokens,
        }
    }
}

impl From<XmlElement> for wit_types::XmlElement {
    fn from(value: XmlElement) -> Self {
        Self {
            namespace: value.namespace.into(),
            local_name: value.local_name,
            attributes: value.attributes.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<XmlAttribute> for wit_types::XmlAttribute {
    fn from(value: XmlAttribute) -> Self {
        Self {
            namespace: value.namespace.map(Into::into),
            local_name: value.local_name,
            value: value.value,
        }
    }
}

impl From<PayloadRoot> for wit_types::PayloadRoot {
    fn from(value: PayloadRoot) -> Self {
        Self {
            namespace: value.namespace.into(),
            local_name: value.local_name,
        }
    }
}

fn push_xml_tokens(element: &XmlElement, tokens: &mut Vec<wit_types::XmlToken>) {
    tokens.push(wit_types::XmlToken::StartElement(wit_types::XmlElement {
        namespace: element.namespace.clone().into(),
        local_name: element.local_name.clone(),
        attributes: element
            .attributes
            .clone()
            .into_iter()
            .map(Into::into)
            .collect(),
    }));
    for child in &element.children {
        match child {
            XmlNode::Element(child) => push_xml_tokens(child, tokens),
            XmlNode::Text(text) => tokens.push(wit_types::XmlToken::Text(text.clone())),
        }
    }
    tokens.push(wit_types::XmlToken::EndElement);
}

macro_rules! impl_domain_newtype_to_wit {
    ($domain:ty, $wit:ident) => {
        impl From<$domain> for wit_types::$wit {
            fn from(value: $domain) -> Self {
                domain_newtype_to_wit!(value, $wit)
            }
        }
    };
}

impl_domain_newtype_to_wit!(ActionId, ActionId);
impl_domain_newtype_to_wit!(CommandNode, CommandNode);
impl_domain_newtype_to_wit!(CommandSessionId, CommandSessionId);
impl_domain_newtype_to_wit!(DisplayText, DisplayText);
impl_domain_newtype_to_wit!(EnrichmentId, EnrichmentId);
impl_domain_newtype_to_wit!(FullJidValue, FullJid);
impl_domain_newtype_to_wit!(LaunchId, LaunchId);
impl_domain_newtype_to_wit!(ListId, ListId);
impl_domain_newtype_to_wit!(ListItemId, ListItemId);
impl_domain_newtype_to_wit!(MediaType, MediaType);
impl_domain_newtype_to_wit!(PayloadNamespace, PayloadNamespace);
impl_domain_newtype_to_wit!(PluginId, PluginId);
impl_domain_newtype_to_wit!(ProviderDeliveryId, ProviderDeliveryId);
impl_domain_newtype_to_wit!(ProviderEventType, ProviderEventType);
impl_domain_newtype_to_wit!(ProviderFieldName, ProviderFieldName);
impl_domain_newtype_to_wit!(ProviderFieldNumber, ProviderFieldNumber);
impl_domain_newtype_to_wit!(ProviderFieldText, ProviderFieldText);
impl_domain_newtype_to_wit!(ProviderId, ProviderId);
impl_domain_newtype_to_wit!(PubSubItemId, PubsubItemId);
impl_domain_newtype_to_wit!(PubSubNode, PubsubNode);
impl_domain_newtype_to_wit!(RoomJid, RoomJid);
impl_domain_newtype_to_wit!(Sha256Digest, Sha256Digest);
impl_domain_newtype_to_wit!(StanzaId, StanzaId);
impl_domain_newtype_to_wit!(ThreadId, ThreadId);
impl_domain_newtype_to_wit!(Timestamp, Timestamp);
impl_domain_newtype_to_wit!(UiActionId, UiActionId);
impl_domain_newtype_to_wit!(UiViewId, UiViewId);
impl_domain_newtype_to_wit!(Url, Url);
impl_domain_newtype_to_wit!(WaddleId, WaddleId);
