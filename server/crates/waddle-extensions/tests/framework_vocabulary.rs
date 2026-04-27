use minidom::Element;
use waddle_extensions::types::*;
use xmpp_parsers::message::Message;

const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn text(value: &str) -> DisplayText {
    DisplayText::new(value).expect("display text should be valid")
}

macro_rules! id {
    ($constructor:path, $value:expr) => {
        $constructor($value).expect("id should be valid")
    };
}

fn artifact() -> ArtifactReference {
    ArtifactReference::new(
        format!("https://artifacts.example.com/links-task-board/sha256/{DIGEST}/thumb.png"),
        DIGEST,
        Some(MediaType::new("image/png").expect("media type should be valid")),
    )
    .expect("artifact reference should be digest pinned")
}

#[test]
fn sample_plugins_use_waddle_namespaces_and_exact_capabilities() {
    for plugin in SamplePlugin::ALL {
        let namespace = plugin.payload_namespace();
        assert!(namespace.as_str().starts_with("urn:waddle:"));
        assert!(!plugin.capabilities().is_empty());
    }

    assert_eq!(
        SamplePlugin::DecisionPolls.payload_namespace().as_str(),
        DECISION_POLLS_NAMESPACE
    );
    assert!(SamplePlugin::AiChatbot
        .capabilities()
        .contains(&ExtensionCapability::AiInvoke));
    assert!(SamplePlugin::LinksTaskBoard
        .capabilities()
        .contains(&ExtensionCapability::MessageEnrich));
}

#[test]
fn namespace_validation_rejects_official_or_non_waddle_semantics() {
    assert!(matches!(
        PayloadNamespace::new("urn:xmpp:sid:0"),
        Err(FrameworkTypeError::OfficialNamespace(_))
    ));
    assert!(matches!(
        PayloadNamespace::new("http://jabber.org/protocol/pubsub"),
        Err(FrameworkTypeError::OfficialNamespace(_))
    ));
    assert!(matches!(
        PayloadNamespace::new("https://example.com/not-xmpp"),
        Err(FrameworkTypeError::NonWaddleNamespace(_))
    ));
}

#[test]
fn command_nodes_must_be_exact_framework_nodes() {
    assert!(CommandNode::new(INVOKE_COMMAND_NODE).is_ok());
    assert!(matches!(
        CommandNode::new("urn:waddle:extension:10:invoke"),
        Err(FrameworkTypeError::InvalidCommandNode(_))
    ));
    assert!(matches!(
        CommandNode::new("http://jabber.org/protocol/commands"),
        Err(FrameworkTypeError::InvalidCommandNode(_))
    ));
}

#[test]
fn artifact_references_must_be_digest_pinned_and_immutable() {
    let reference = artifact();
    assert_eq!(reference.sha256.as_str(), DIGEST);

    assert!(matches!(
        ArtifactReference::new(
            "https://artifacts.example.com/canvas/latest/render.png",
            DIGEST,
            None
        ),
        Err(FrameworkTypeError::InvalidArtifactUri(_))
    ));
    assert!(matches!(
        ArtifactReference::new(
            format!("https://artifacts.example.com/canvas/sha256/{DIGEST}/render.png"),
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            None,
        ),
        Err(FrameworkTypeError::ArtifactDigestMismatch { .. })
    ));
}

#[test]
fn enrichment_rejects_mismatched_plugin_payload_namespaces() {
    let good = MessageEnrichment {
        id: id!(EnrichmentId::new, "enrich-1"),
        plugin: id!(PluginId::new, "links-task-board"),
        capability: ExtensionCapability::MessageEnrich,
        payload_namespace: PayloadNamespace::links_task_board(),
        created_at: id!(Timestamp::new, "2026-04-27T10:00:00Z"),
        source: Some(MessageSource {
            stanza_id: id!(StanzaId::new, "archive-id-456"),
            body_range: Some(BodyRange::new(5, 29).expect("range should be valid")),
        }),
        payloads: vec![FrameworkPayload::LinkPreview(LinkPreview {
            url: id!(Url::new, "https://example.org/post"),
            title: Some(text("Example Post")),
            site: Some(text("Example")),
            image: Some(artifact()),
        })],
        launches: vec![LaunchDescriptor {
            id: id!(LaunchId::new, "save-link"),
            plugin: id!(PluginId::new, "links-task-board"),
            action: id!(ActionId::new, "save-link"),
            command_node: CommandNode::invoke(),
            label: text("Save link"),
            context: LaunchContext {
                waddle_id: id!(WaddleId::new, "waddle-123"),
                source_stanza_id: Some(id!(StanzaId::new, "archive-id-456")),
            },
            payload: Some(LaunchPayload::SaveLink(SaveLinkRequest {
                url: id!(Url::new, "https://example.org/post"),
                collection_id: None,
            })),
            expires_at: None,
        }],
    };

    assert!(good.payloads_match_declared_namespace());

    let bad = MessageEnrichment {
        payloads: vec![FrameworkPayload::QuizQuestion(QuizQuestion {
            game_id: id!(GameId::new, "game-1"),
            question_id: id!(QuestionId::new, "q1"),
            prompt: text("Which XEP defines Ad-Hoc Commands?"),
            choices: vec![QuizChoice {
                id: id!(OptionId::new, "b"),
                label: text("XEP-0050"),
            }],
            closes_at: None,
        })],
        launches: Vec::new(),
        ..good
    };

    assert!(!bad.payloads_match_declared_namespace());
}

#[test]
fn framework_envelope_builds_typed_xmpp_payload() {
    let envelope = ExtensionEnvelope::new(vec![MessageEnrichment {
        id: id!(EnrichmentId::new, "poll-enrich-1"),
        plugin: id!(PluginId::new, "decision-polls"),
        capability: ExtensionCapability::BotRespond,
        payload_namespace: PayloadNamespace::decision_polls(),
        created_at: id!(Timestamp::new, "2026-04-27T10:00:00Z"),
        source: None,
        payloads: vec![FrameworkPayload::DecisionPoll(DecisionPoll {
            poll_id: id!(OptionId::new, "poll-1"),
            mode: PollMode::Single,
            question: text("Ship the extension framework this week?"),
            options: vec![
                PollOption {
                    id: id!(OptionId::new, "yes"),
                    label: text("Yes"),
                },
                PollOption {
                    id: id!(OptionId::new, "no"),
                    label: text("No"),
                },
            ],
            closes_at: Some(id!(Timestamp::new, "2026-04-27T11:00:00Z")),
        })],
        launches: vec![LaunchDescriptor {
            id: id!(LaunchId::new, "vote-yes"),
            plugin: id!(PluginId::new, "decision-polls"),
            action: id!(ActionId::new, "vote"),
            command_node: CommandNode::invoke(),
            label: text("Vote yes"),
            context: LaunchContext {
                waddle_id: id!(WaddleId::new, "waddle-123"),
                source_stanza_id: Some(id!(StanzaId::new, "archive-id-poll-1")),
            },
            payload: Some(LaunchPayload::PollVote(PollVoteRequest {
                poll_id: id!(OptionId::new, "poll-1"),
                option_id: id!(OptionId::new, "yes"),
            })),
            expires_at: None,
        }],
    }]);

    let element = envelope.to_minidom();
    assert!(element.is("extensions", FRAMEWORK_NAMESPACE));
    assert_eq!(element.attr("version"), Some("1"));

    let enrichment = element
        .get_child("enrichment", FRAMEWORK_NAMESPACE)
        .expect("enrichment child should exist");
    assert_eq!(enrichment.attr("plugin"), Some("decision-polls"));
    assert_eq!(
        enrichment.attr("payload-ns"),
        Some(DECISION_POLLS_NAMESPACE)
    );

    let payload = enrichment
        .get_child("payload", FRAMEWORK_NAMESPACE)
        .expect("payload child should exist");
    assert!(payload
        .get_child("poll", DECISION_POLLS_NAMESPACE)
        .is_some());

    let launch = enrichment
        .get_child("launch", FRAMEWORK_NAMESPACE)
        .expect("launch child should exist");
    assert_eq!(launch.attr("command-node"), Some(INVOKE_COMMAND_NODE));
    let launch_payload = launch
        .get_child("payload", FRAMEWORK_NAMESPACE)
        .expect("launch payload child should exist");
    assert!(launch_payload
        .get_child("vote-request", DECISION_POLLS_NAMESPACE)
        .is_some());
}

#[test]
fn framework_payloads_cover_ui_list_board_ai_bot_and_canvas() {
    let canvas_artifact = ArtifactReference::new(
        format!("https://artifacts.example.com/ai-assistant-canvas/sha256/{DIGEST}/render.png"),
        DIGEST,
        Some(MediaType::new("image/png").expect("media type should be valid")),
    )
    .expect("canvas artifact should be digest pinned");

    let payloads = vec![
        FrameworkPayload::DeclarativeUi(UiView {
            id: id!(UiViewId::new, "assistant-panel"),
            title: Some(text("Assistant")),
            blocks: vec![
                UiBlock::Text(TextBlock {
                    text: text("Ready"),
                    style: TextStyle::Body,
                }),
                UiBlock::Action(ActionBlock {
                    launch_id: id!(LaunchId::new, "ask-followup"),
                    label: text("Ask follow-up"),
                }),
            ],
        }),
        FrameworkPayload::List(ListView {
            id: id!(ListId::new, "saved-links"),
            title: Some(text("Saved links")),
            items: vec![ListItem {
                id: id!(ListItemId::new, "link-1"),
                label: text("Example Post"),
                description: Some(text("A saved link")),
                image: None,
                launch_id: Some(id!(LaunchId::new, "save-link")),
            }],
        }),
        FrameworkPayload::Board(BoardView {
            id: id!(BoardId::new, "tasks"),
            title: Some(text("Tasks")),
            columns: vec![BoardColumn {
                id: id!(BoardColumnId::new, "todo"),
                title: text("Todo"),
                cards: vec![BoardCard {
                    id: id!(BoardCardId::new, "card-1"),
                    title: text("Write tests"),
                    body: Some(text("Cover framework vocabulary")),
                    labels: vec![text("server")],
                    launch_id: None,
                }],
            }],
        }),
        FrameworkPayload::AssistantAnswer(AssistantAnswer {
            run_id: id!(RunId::new, "run-1"),
            profile: id!(ProfileId::new, "default"),
            context_source: AssistantContextSource::Mam,
            summary: Some(text("Answer ready")),
        }),
        FrameworkPayload::CanvasRender(CanvasRender {
            canvas_id: id!(CanvasId::new, "canvas-1"),
            render_id: id!(RenderId::new, "render-1"),
            artifact: canvas_artifact,
        }),
    ];

    assert_eq!(payloads[0].payload_namespace(), FRAMEWORK_NAMESPACE);
    assert_eq!(payloads[1].payload_namespace(), FRAMEWORK_NAMESPACE);
    assert_eq!(payloads[2].payload_namespace(), FRAMEWORK_NAMESPACE);
    assert_eq!(payloads[3].payload_namespace(), AI_CHATBOT_NAMESPACE);
    assert_eq!(
        payloads[4].payload_namespace(),
        AI_ASSISTANT_CANVAS_NAMESPACE
    );

    let response = ExtensionResponse {
        effects: vec![ExtensionEffect::SendBotMessage(BotMessage {
            body: text("Here is the current workspace"),
            payloads,
            launches: Vec::new(),
        })],
    };

    let ExtensionEffect::SendBotMessage(message) = &response.effects[0] else {
        panic!("effect should be a bot message");
    };
    assert_eq!(message.payloads.len(), 5);
    assert!(message.payloads[0]
        .to_minidom()
        .is("view", FRAMEWORK_NAMESPACE));
    assert!(message.payloads[4]
        .to_minidom()
        .is("canvas", AI_ASSISTANT_CANVAS_NAMESPACE));
}

#[test]
fn declarative_text_is_not_parsed_as_xml() {
    let payload = FrameworkPayload::QuizQuestion(QuizQuestion {
        game_id: id!(GameId::new, "game-1"),
        question_id: id!(QuestionId::new, "q1"),
        prompt: text("<script/> is literal text"),
        choices: vec![QuizChoice {
            id: id!(OptionId::new, "a"),
            label: text("<choice/> is literal text"),
        }],
        closes_at: None,
    });

    let element = payload.to_minidom();
    let prompt = element
        .get_child("prompt", PUB_QUIZ_NAMESPACE)
        .expect("prompt child should exist");
    assert_eq!(prompt.children().count(), 0);
    assert_eq!(prompt.text(), "<script/> is literal text");

    let choice = element
        .get_child("choice", PUB_QUIZ_NAMESPACE)
        .expect("choice child should exist");
    assert_eq!(choice.children().count(), 0);
    assert_eq!(choice.text(), "<choice/> is literal text");
}

#[test]
fn framework_envelope_detection_is_exact() {
    let mut message = Message::new(None);
    message
        .payloads
        .push(Element::builder("extensions", FRAMEWORK_NAMESPACE).build());
    assert!(message_has_framework_envelope(&message));

    let mut official_message = Message::new(None);
    official_message
        .payloads
        .push(Element::builder("extensions", "urn:xmpp:sid:0").build());
    assert!(!message_has_framework_envelope(&official_message));
}

#[test]
fn legacy_embed_children_are_text_not_xml() {
    let embed = EmbedElement {
        element_name: "extensions".to_string(),
        namespace: FRAMEWORK_NAMESPACE.to_string(),
        attributes: Vec::new(),
        children: vec!["<poll xmlns='urn:waddle:decision-polls:1'/>".to_string()],
    };

    let element = embed.to_minidom();
    assert_eq!(element.children().count(), 0);
    assert_eq!(
        element.text(),
        "<poll xmlns='urn:waddle:decision-polls:1'/>"
    );
}
