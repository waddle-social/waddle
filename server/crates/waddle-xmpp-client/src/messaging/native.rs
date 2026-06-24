use minidom::Element;

use crate::client::ClientHandle;
use crate::error::ClientResult;
use crate::request::StanzaId;

use super::builders::{
    build_chat_state_message, build_displayed_message, build_outbound_message,
    build_retraction_message,
};
use super::namespaces::*;
use super::types::SendMessageOptions;
use crate::xep::thread::ThreadRef;

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub trait MessagingExt {
    fn join_room<'a>(
        &'a self,
        room_jid: &'a str,
        nick: &'a str,
    ) -> impl std::future::Future<Output = ClientResult<()>> + Send + 'a;
    fn leave_room<'a>(
        &'a self,
        room_jid: &'a str,
        nick: &'a str,
    ) -> impl std::future::Future<Output = ClientResult<()>> + Send + 'a;
    fn send_groupchat_message<'a>(
        &'a self,
        room_jid: &'a str,
        body: &'a str,
        options: &'a SendMessageOptions,
    ) -> impl std::future::Future<Output = ClientResult<StanzaId>> + Send + 'a;
    fn send_chat_message<'a>(
        &'a self,
        peer_jid: &'a str,
        body: &'a str,
        options: &'a SendMessageOptions,
    ) -> impl std::future::Future<Output = ClientResult<StanzaId>> + Send + 'a;
    fn send_presence<'a>(
        &'a self,
        status: Option<&'a str>,
        show: Option<&'a str>,
    ) -> impl std::future::Future<Output = ClientResult<()>> + Send + 'a;
    fn send_chat_state<'a>(
        &'a self,
        jid: &'a str,
        state: &'a str,
        message_type: &'a str,
        thread: Option<&'a ThreadRef>,
    ) -> impl std::future::Future<Output = ClientResult<()>> + Send + 'a;
    fn send_displayed_marker<'a>(
        &'a self,
        jid: &'a str,
        message_id: &'a str,
        message_type: &'a str,
        thread: Option<&'a ThreadRef>,
    ) -> impl std::future::Future<Output = ClientResult<()>> + Send + 'a;
    fn retract_message<'a>(
        &'a self,
        jid: &'a str,
        message_id: &'a str,
        message_type: &'a str,
        thread: Option<&'a ThreadRef>,
    ) -> impl std::future::Future<Output = ClientResult<()>> + Send + 'a;
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
impl MessagingExt for ClientHandle {
    async fn join_room(&self, room_jid: &str, nick: &str) -> ClientResult<()> {
        let to = format!("{}/{}", room_jid, nick);
        let stanza = Element::builder("presence", NS_CLIENT)
            .attr(minidom::rxml::xml_ncname!("to").to_owned(), to.as_str())
            .append(Element::builder("x", NS_MUC).build())
            .build();
        self.send_stanza(stanza).await
    }

    async fn leave_room(&self, room_jid: &str, nick: &str) -> ClientResult<()> {
        let to = format!("{}/{}", room_jid, nick);
        let stanza = Element::builder("presence", NS_CLIENT)
            .attr(minidom::rxml::xml_ncname!("to").to_owned(), to.as_str())
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "unavailable")
            .build();
        self.send_stanza(stanza).await
    }

    async fn send_groupchat_message(
        &self,
        room_jid: &str,
        body: &str,
        options: &SendMessageOptions,
    ) -> ClientResult<StanzaId> {
        let (stanza_id, stanza) = build_outbound_message(room_jid, "groupchat", body, options)?;
        self.send_stanza(stanza).await?;
        Ok(stanza_id)
    }

    async fn send_chat_message(
        &self,
        peer_jid: &str,
        body: &str,
        options: &SendMessageOptions,
    ) -> ClientResult<StanzaId> {
        let (stanza_id, stanza) = build_outbound_message(peer_jid, "chat", body, options)?;
        self.send_stanza(stanza).await?;
        Ok(stanza_id)
    }

    async fn send_presence(&self, status: Option<&str>, show: Option<&str>) -> ClientResult<()> {
        let presence = super::presence::build_presence_stanza(
            status,
            show.and_then(super::presence::parse_show),
        );
        self.send_stanza(presence.into()).await
    }

    async fn send_chat_state(
        &self,
        jid: &str,
        state: &str,
        message_type: &str,
        thread: Option<&ThreadRef>,
    ) -> ClientResult<()> {
        let stanza = build_chat_state_message(jid, state, message_type, thread)?;
        self.send_stanza(stanza).await
    }

    async fn send_displayed_marker(
        &self,
        jid: &str,
        message_id: &str,
        message_type: &str,
        thread: Option<&ThreadRef>,
    ) -> ClientResult<()> {
        let stanza = build_displayed_message(jid, message_id, message_type, thread);
        self.send_stanza(stanza).await
    }

    async fn retract_message(
        &self,
        jid: &str,
        message_id: &str,
        message_type: &str,
        thread: Option<&ThreadRef>,
    ) -> ClientResult<()> {
        let stanza = build_retraction_message(jid, message_type, message_id, thread);
        self.send_stanza(stanza).await
    }
}
