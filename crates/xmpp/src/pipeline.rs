use tracing::{debug, warn};

use crate::{error::PipelineError, stanza::Stanza};

pub enum ProcessorResult {
    Continue,
    Drop,
    Replace(Box<Stanza>),
}

pub struct ProcessorContext {
    pub direction: StanzaDirection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StanzaDirection {
    Inbound,
    Outbound,
}

pub trait StanzaProcessor: Send + Sync + 'static {
    fn name(&self) -> &str;

    fn process_inbound(&self, stanza: &mut Stanza, ctx: &ProcessorContext) -> ProcessorResult;

    fn process_outbound(&self, stanza: &mut Stanza, ctx: &ProcessorContext) -> ProcessorResult;

    fn priority(&self) -> i32;
}

pub struct StanzaPipeline {
    processors: Vec<Box<dyn StanzaProcessor>>,
}

impl StanzaPipeline {
    pub fn new() -> Self {
        Self {
            processors: Vec::new(),
        }
    }

    pub fn register(&mut self, processor: Box<dyn StanzaProcessor>) {
        self.processors.push(processor);
        self.processors.sort_by_key(|p| p.priority());
    }

    pub fn processor_count(&self) -> usize {
        self.processors.len()
    }

    pub async fn process_inbound(&self, raw: &[u8]) -> Result<(), PipelineError> {
        let mut stanza = Stanza::parse(raw)?;

        debug!(
            stanza_type = stanza.name(),
            "inbound stanza entering pipeline"
        );

        let ctx = ProcessorContext {
            direction: StanzaDirection::Inbound,
        };

        for processor in &self.processors {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                processor.process_inbound(&mut stanza, &ctx)
            })) {
                Ok(ProcessorResult::Continue) => {}
                Ok(ProcessorResult::Drop) => {
                    debug!(
                        processor = processor.name(),
                        stanza_type = stanza.name(),
                        "inbound stanza dropped by processor"
                    );
                    return Ok(());
                }
                Ok(ProcessorResult::Replace(replacement)) => {
                    debug!(
                        processor = processor.name(),
                        old_type = stanza.name(),
                        new_type = replacement.name(),
                        "inbound stanza replaced by processor"
                    );
                    stanza = *replacement;
                }
                Err(_) => {
                    warn!(
                        processor = processor.name(),
                        stanza_type = stanza.name(),
                        "processor panicked during inbound processing, skipping"
                    );
                }
            }
        }

        Ok(())
    }

    pub async fn process_outbound(&self, mut stanza: Stanza) -> Result<Vec<u8>, PipelineError> {
        debug!(
            stanza_type = stanza.name(),
            "outbound stanza entering pipeline"
        );

        let ctx = ProcessorContext {
            direction: StanzaDirection::Outbound,
        };

        for processor in &self.processors {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                processor.process_outbound(&mut stanza, &ctx)
            })) {
                Ok(ProcessorResult::Continue) => {}
                Ok(ProcessorResult::Drop) => {
                    debug!(
                        processor = processor.name(),
                        stanza_type = stanza.name(),
                        "outbound stanza dropped by processor"
                    );
                    return Err(PipelineError::ProcessorFailed(format!(
                        "outbound stanza dropped by processor '{}'",
                        processor.name()
                    )));
                }
                Ok(ProcessorResult::Replace(replacement)) => {
                    debug!(
                        processor = processor.name(),
                        old_type = stanza.name(),
                        new_type = replacement.name(),
                        "outbound stanza replaced by processor"
                    );
                    stanza = *replacement;
                }
                Err(_) => {
                    warn!(
                        processor = processor.name(),
                        stanza_type = stanza.name(),
                        "processor panicked during outbound processing, skipping"
                    );
                }
            }
        }

        stanza.to_bytes()
    }
}

impl Default for StanzaPipeline {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    struct TrackingProcessor {
        prio: i32,
        inbound_counter: &'static AtomicU32,
        outbound_counter: &'static AtomicU32,
    }

    impl StanzaProcessor for TrackingProcessor {
        fn name(&self) -> &str {
            "tracker"
        }

        fn process_inbound(
            &self,
            _stanza: &mut Stanza,
            _ctx: &ProcessorContext,
        ) -> ProcessorResult {
            self.inbound_counter.fetch_add(1, Ordering::SeqCst);
            ProcessorResult::Continue
        }

        fn process_outbound(
            &self,
            _stanza: &mut Stanza,
            _ctx: &ProcessorContext,
        ) -> ProcessorResult {
            self.outbound_counter.fetch_add(1, Ordering::SeqCst);
            ProcessorResult::Continue
        }

        fn priority(&self) -> i32 {
            self.prio
        }
    }

    struct PassthroughProcessor {
        prio: i32,
    }

    impl StanzaProcessor for PassthroughProcessor {
        fn name(&self) -> &str {
            "passthrough"
        }

        fn process_inbound(
            &self,
            _stanza: &mut Stanza,
            _ctx: &ProcessorContext,
        ) -> ProcessorResult {
            ProcessorResult::Continue
        }

        fn process_outbound(
            &self,
            _stanza: &mut Stanza,
            _ctx: &ProcessorContext,
        ) -> ProcessorResult {
            ProcessorResult::Continue
        }

        fn priority(&self) -> i32 {
            self.prio
        }
    }

    struct DroppingProcessor {
        prio: i32,
    }

    impl StanzaProcessor for DroppingProcessor {
        fn name(&self) -> &str {
            "dropper"
        }

        fn process_inbound(
            &self,
            _stanza: &mut Stanza,
            _ctx: &ProcessorContext,
        ) -> ProcessorResult {
            ProcessorResult::Drop
        }

        fn process_outbound(
            &self,
            _stanza: &mut Stanza,
            _ctx: &ProcessorContext,
        ) -> ProcessorResult {
            ProcessorResult::Drop
        }

        fn priority(&self) -> i32 {
            self.prio
        }
    }

    struct ReplacingProcessor;

    impl StanzaProcessor for ReplacingProcessor {
        fn name(&self) -> &str {
            "replacer"
        }

        fn process_inbound(
            &self,
            _stanza: &mut Stanza,
            _ctx: &ProcessorContext,
        ) -> ProcessorResult {
            let presence_xml = b"<presence xmlns='jabber:client'><show>away</show></presence>";
            let replacement = Stanza::parse(presence_xml).expect("test stanza should parse");
            ProcessorResult::Replace(Box::new(replacement))
        }

        fn process_outbound(
            &self,
            _stanza: &mut Stanza,
            _ctx: &ProcessorContext,
        ) -> ProcessorResult {
            ProcessorResult::Continue
        }

        fn priority(&self) -> i32 {
            5
        }
    }

    struct PanickingProcessor;

    impl StanzaProcessor for PanickingProcessor {
        fn name(&self) -> &str {
            "panicker"
        }

        fn process_inbound(
            &self,
            _stanza: &mut Stanza,
            _ctx: &ProcessorContext,
        ) -> ProcessorResult {
            panic!("processor panic for testing");
        }

        fn process_outbound(
            &self,
            _stanza: &mut Stanza,
            _ctx: &ProcessorContext,
        ) -> ProcessorResult {
            panic!("processor panic for testing");
        }

        fn priority(&self) -> i32 {
            15
        }
    }

    const MESSAGE_XML: &[u8] = b"<message xmlns='jabber:client' type='chat' from='alice@example.com' to='bob@example.com'><body>hello</body></message>";
    const PRESENCE_XML: &[u8] =
        b"<presence xmlns='jabber:client'><show>away</show><status>out</status></presence>";

    #[tokio::test]
    async fn processors_execute_in_priority_order() {
        let mut pipeline = StanzaPipeline::new();
        pipeline.register(Box::new(PassthroughProcessor { prio: 100 }));
        pipeline.register(Box::new(PassthroughProcessor { prio: 5 }));
        pipeline.register(Box::new(PassthroughProcessor { prio: 20 }));

        assert_eq!(pipeline.processor_count(), 3);

        pipeline
            .process_inbound(MESSAGE_XML)
            .await
            .expect("inbound should succeed");

        let priorities: Vec<i32> = pipeline.processors.iter().map(|p| p.priority()).collect();
        assert_eq!(priorities, vec![5, 20, 100]);
    }

    #[tokio::test]
    async fn drop_result_stops_inbound_pipeline() {
        static AFTER_INBOUND: AtomicU32 = AtomicU32::new(0);
        static AFTER_OUTBOUND: AtomicU32 = AtomicU32::new(0);
        AFTER_INBOUND.store(0, Ordering::SeqCst);
        AFTER_OUTBOUND.store(0, Ordering::SeqCst);

        let mut pipeline = StanzaPipeline::new();
        pipeline.register(Box::new(DroppingProcessor { prio: 5 }));
        pipeline.register(Box::new(TrackingProcessor {
            prio: 10,
            inbound_counter: &AFTER_INBOUND,
            outbound_counter: &AFTER_OUTBOUND,
        }));

        pipeline
            .process_inbound(MESSAGE_XML)
            .await
            .expect("inbound should succeed");

        assert_eq!(
            AFTER_INBOUND.load(Ordering::SeqCst),
            0,
            "processor after drop should not run"
        );
    }

    #[tokio::test]
    async fn drop_result_returns_error_on_outbound() {
        let mut pipeline = StanzaPipeline::new();
        pipeline.register(Box::new(DroppingProcessor { prio: 5 }));

        let result = pipeline
            .process_outbound(Stanza::parse(MESSAGE_XML).unwrap())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn replace_result_substitutes_stanza() {
        static AFTER_INBOUND: AtomicU32 = AtomicU32::new(0);
        static AFTER_OUTBOUND: AtomicU32 = AtomicU32::new(0);
        AFTER_INBOUND.store(0, Ordering::SeqCst);
        AFTER_OUTBOUND.store(0, Ordering::SeqCst);

        let mut pipeline = StanzaPipeline::new();
        pipeline.register(Box::new(ReplacingProcessor));
        pipeline.register(Box::new(TrackingProcessor {
            prio: 10,
            inbound_counter: &AFTER_INBOUND,
            outbound_counter: &AFTER_OUTBOUND,
        }));

        pipeline
            .process_inbound(MESSAGE_XML)
            .await
            .expect("inbound should succeed");

        assert_eq!(
            AFTER_INBOUND.load(Ordering::SeqCst),
            1,
            "observer should still run after replace"
        );
    }

    #[tokio::test]
    async fn panicking_processor_is_skipped() {
        static BEFORE_INBOUND: AtomicU32 = AtomicU32::new(0);
        static BEFORE_OUTBOUND: AtomicU32 = AtomicU32::new(0);
        static AFTER_INBOUND: AtomicU32 = AtomicU32::new(0);
        static AFTER_OUTBOUND: AtomicU32 = AtomicU32::new(0);
        BEFORE_INBOUND.store(0, Ordering::SeqCst);
        BEFORE_OUTBOUND.store(0, Ordering::SeqCst);
        AFTER_INBOUND.store(0, Ordering::SeqCst);
        AFTER_OUTBOUND.store(0, Ordering::SeqCst);

        let mut pipeline = StanzaPipeline::new();
        pipeline.register(Box::new(TrackingProcessor {
            prio: 10,
            inbound_counter: &BEFORE_INBOUND,
            outbound_counter: &BEFORE_OUTBOUND,
        }));
        pipeline.register(Box::new(PanickingProcessor));
        pipeline.register(Box::new(TrackingProcessor {
            prio: 20,
            inbound_counter: &AFTER_INBOUND,
            outbound_counter: &AFTER_OUTBOUND,
        }));

        pipeline
            .process_inbound(MESSAGE_XML)
            .await
            .expect("inbound should succeed despite panic");

        assert_eq!(BEFORE_INBOUND.load(Ordering::SeqCst), 1);
        assert_eq!(AFTER_INBOUND.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn parse_failure_returns_error() {
        let pipeline = StanzaPipeline::new();
        let result = pipeline.process_inbound(b"not xml at all<<<").await;
        assert!(matches!(result, Err(PipelineError::ParseFailed(_))));
    }

    #[tokio::test]
    async fn outbound_serializes_stanza() {
        let pipeline = StanzaPipeline::new();
        let stanza = Stanza::parse(PRESENCE_XML).expect("should parse");

        let bytes = pipeline
            .process_outbound(stanza)
            .await
            .expect("outbound should succeed");

        let round_tripped = Stanza::parse(&bytes).expect("should re-parse");
        assert!(matches!(round_tripped, Stanza::Presence(_)));
    }

    #[tokio::test]
    async fn plugin_pre_process_hook_runs_before_core_processors() {
        let mut pipeline = StanzaPipeline::new();
        pipeline.register(Box::new(PassthroughProcessor { prio: 10 }));
        pipeline.register(Box::new(PassthroughProcessor { prio: 5 }));
        pipeline.register(Box::new(PassthroughProcessor { prio: 60 }));

        let priorities: Vec<i32> = pipeline.processors.iter().map(|p| p.priority()).collect();
        assert_eq!(
            priorities,
            vec![5, 10, 60],
            "plugin pre-process (5) < core (10) < plugin post-process (60)"
        );

        pipeline
            .process_inbound(MESSAGE_XML)
            .await
            .expect("inbound should succeed");
    }

    #[tokio::test]
    async fn register_maintains_sorted_order() {
        let mut pipeline = StanzaPipeline::new();
        pipeline.register(Box::new(PassthroughProcessor { prio: 50 }));
        pipeline.register(Box::new(PassthroughProcessor { prio: 1 }));
        pipeline.register(Box::new(PassthroughProcessor { prio: 25 }));
        pipeline.register(Box::new(PassthroughProcessor { prio: 10 }));

        let priorities: Vec<i32> = pipeline.processors.iter().map(|p| p.priority()).collect();
        assert_eq!(priorities, vec![1, 10, 25, 50]);
    }

    #[tokio::test]
    async fn empty_pipeline_passes_through() {
        let pipeline = StanzaPipeline::new();

        pipeline
            .process_inbound(MESSAGE_XML)
            .await
            .expect("empty pipeline inbound should succeed");

        let stanza = Stanza::parse(MESSAGE_XML).unwrap();
        let bytes = pipeline
            .process_outbound(stanza)
            .await
            .expect("empty pipeline outbound should succeed");

        let round_tripped = Stanza::parse(&bytes).expect("should re-parse");
        assert!(matches!(round_tripped, Stanza::Message(_)));
    }
}
