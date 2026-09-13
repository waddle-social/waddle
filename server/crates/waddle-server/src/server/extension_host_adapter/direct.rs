//! Direct host dispatch enters the same durable authority as connected senders.
use std::{sync::Arc, time::Duration};

use jid::Jid;
use waddle_extensions::{ReplyTarget, StanzaId, ThreadId};
use waddle_xmpp::{
    ingress::{DigestContext, NormalizedTarget, TransportGeneration},
    protocol::{Blocklist, XmppStateMachine},
};
use xmpp_parsers::message::{Message, MessageType};

use crate::ingress::{
    nested::{NestedContinuation, NestedOutcome, NestedRefusal},
    submission::{digest_authorities, digest_input},
    ExtensionPrincipal, IngressDecisionClass, IngressPrincipal, IngressStreamIdentity,
    IngressSubmission,
};

use super::{interpret, ExtensionHostAdapter, ExtensionHostAdapterError, ExtensionInvocation};

/// A host response may time out while authority-owned settlement continues.
const SETTLEMENT_RESPONSE_DEADLINE: Duration = Duration::from_secs(2);

pub(super) struct DirectDispatchMessage {
    pub stanza_id: StanzaId,
    pub body: String,
    pub thread_id: Option<ThreadId>,
    pub reply_to: Option<ReplyTarget>,
    pub markup: Vec<waddle_extensions::MessageMarkupSpan>,
}

impl ExtensionHostAdapter {
    pub(super) async fn dispatch_direct(
        &self,
        invocation: &ExtensionInvocation,
        target: Jid,
        request: DirectDispatchMessage,
    ) -> Result<(), ExtensionHostAdapterError> {
        let authority = &self.state.deps.protocol.ingress;
        let grant = authority
            .active_extension_send_grant(&invocation.plugin_id)
            .await
            .map_err(|error| ExtensionHostAdapterError::Storage(error.to_string()))?
            .ok_or(ExtensionHostAdapterError::NotAuthorized)?;
        let operation = authority
            .try_begin_nested()
            .map_err(|error| ExtensionHostAdapterError::Storage(error.to_string()))?;
        let requester = invocation.actor_jid.to_bare();
        let sender = requester
            .with_resource_str("extension-host")
            .map_err(|error| ExtensionHostAdapterError::Protocol(error.to_string()))?;
        let normalized_target = match target.try_as_full() {
            Ok(full) => NormalizedTarget::Full(full.clone()),
            Err(bare) => NormalizedTarget::Bare(bare.clone()),
        };
        let message = direct_message(target, request);
        let muc: jid::BareJid = self
            .state
            .deps
            .service_domains
            .muc
            .parse()
            .map_err(|error: jid::Error| ExtensionHostAdapterError::Protocol(error.to_string()))?;
        let digest = digest_input(
            &message,
            &DigestContext {
                target: normalized_target.clone(),
                server_authorities: digest_authorities(&message, &requester, muc.domain()),
                stanza_lang: None,
            },
        )
        .map_err(|error| ExtensionHostAdapterError::Unsupported(error.to_string()))?;
        let mut machine = XmppStateMachine::new(
            self.state.deps.auth_state.xmpp_domain.clone(),
            (*self.state.deps.protocol.dispatcher).clone(),
        );
        machine.transition_to_ready(sender.clone(), false);
        let blocklist = self
            .state
            .deps
            .protocol
            .blocking_storage
            .list_blocked_jid_entries(&requester)
            .await
            .map_err(|error| ExtensionHostAdapterError::Storage(error.to_string()))?;
        machine.set_blocklist(Blocklist::new(blocklist));
        let mut deps = self.interpret_deps(invocation.session.as_ref());
        deps.host_sender = Some(sender.clone());
        let plan = interpret::plan_message_dispatch(&mut machine, message, &deps).await;
        if let Some(failure) = plan.failure {
            return Err(ExtensionHostAdapterError::Unsupported(format!(
                "{failure:?}"
            )));
        }
        let submission = IngressSubmission {
            identity: IngressStreamIdentity::Extension {
                plugin: invocation.plugin_id.clone(),
                requester: Some(requester.clone()),
            },
            principal: IngressPrincipal::Extension(ExtensionPrincipal {
                grant,
                requester: Some(requester.clone()),
                sender: requester,
            }),
            sender,
            target: normalized_target,
            digest_input: digest,
            plan,
            connection_generation: TransportGeneration::Host,
        };
        let continuation =
            NestedContinuation::new(Arc::clone(&self.state), invocation.session.clone());
        match operation
            .commit_and_continue(submission, continuation)
            .await
        {
            NestedOutcome::Refused(NestedRefusal::Decision(
                IngressDecisionClass::PrincipalMissing,
            )) => Err(ExtensionHostAdapterError::NotAuthorized),
            NestedOutcome::Refused(reason) => Err(ExtensionHostAdapterError::Storage(format!(
                "nested ingress refused: {reason:?}"
            ))),
            NestedOutcome::Committed { settlement, .. } => {
                // A dropped waiter cannot cancel the authority's task. Once committed,
                // timeout or persistence failure means acceptance, never a retry request.
                if let Ok(Ok(outcome)) =
                    tokio::time::timeout(SETTLEMENT_RESPONSE_DEADLINE, settlement).await
                {
                    if outcome.terminal.is_ok() {
                        if let Some(rejection) = outcome.rejection {
                            return Err(ExtensionHostAdapterError::Rejected(Box::new(rejection)));
                        }
                    }
                }
                Ok(())
            }
        }
    }
}

fn direct_message(target: Jid, request: DirectDispatchMessage) -> Message {
    let mut message = Message::new(Some(target));
    message.id = Some(xmpp_parsers::message::Id(
        request.stanza_id.as_str().to_owned(),
    ));
    waddle_xmpp_core::xep0359::add_origin_id(&mut message, request.stanza_id.as_str());
    message.type_ = MessageType::Chat;
    message
        .bodies
        .insert(xmpp_parsers::message::Lang::new(), request.body);
    if let Some(thread) = request.thread_id {
        waddle_xmpp::xep0201::set_thread_id(&mut message, thread.as_str());
    }
    if let Some(reply) = request.reply_to {
        let mut reference = waddle_xmpp::xep::ReplyReference::new(reply.id.as_str());
        if let Some(to) = reply
            .to
            .as_ref()
            .and_then(|to| to.as_str().parse::<Jid>().ok())
        {
            reference = reference.with_to(to);
        }
        waddle_xmpp::xep::set_reply_payload(&mut message, &reference);
    }
    if let Some(markup) = interpret::build_extension_message_markup(&request.markup) {
        message.payloads.push(markup);
    }
    message
}
