use crate::{error::PipelineError, stanza::Stanza};

pub enum ProcessorResult {
    Continue,
    Drop,
    Replace(Box<Stanza>),
}

pub struct ProcessorContext {
    pub direction: StanzaDirection,
}

pub enum StanzaDirection {
    Inbound,
    Outbound,
}

pub trait StanzaProcessor: Send + Sync + 'static {
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

    pub async fn process_inbound(&self, _raw: &[u8]) -> Result<(), PipelineError> {
        todo!("StanzaPipeline::process_inbound")
    }

    pub async fn process_outbound(&self, _stanza: Stanza) -> Result<(), PipelineError> {
        todo!("StanzaPipeline::process_outbound")
    }
}

impl Default for StanzaPipeline {
    fn default() -> Self {
        Self::new()
    }
}
