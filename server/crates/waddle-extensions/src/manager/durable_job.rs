use super::*;

impl ExtensionManager {
    /// The extension currently loaded and granted to process durable jobs of
    /// `kind`, if any.
    ///
    /// This is the live source of truth for both:
    /// - the enqueue gate (a caller must not enqueue a job of a kind nothing
    ///   currently holds the grant to process, or rows would pile up in the
    ///   queue with no consumer — see `waddle-server::extension_job_outbox`'s
    ///   module docs for why a static config flag is the wrong gate); and
    /// - job routing at drain time (the drain worker looks the destination
    ///   extension back up by the plugin id recorded on the row at enqueue
    ///   time; if that extension has since been unloaded or had its grant
    ///   revoked, this returns `None` and the row is retried like any other
    ///   transient failure instead of being force-routed to a different
    ///   extension).
    ///
    /// Matching is capability-and-declared-kind together: an extension must
    /// declare and be granted [`ExtensionCapability::DurableJob`] *and* list
    /// `kind` in its manifest's `durable_job_kinds` — either alone is not
    /// enough, mirroring every other effect's
    /// `declares_capability(..) && grants.contains(..)` check.
    ///
    /// Integration coverage (a loaded/granted extension is found; an
    /// unloaded, ungranted, or wrong-job-kind extension is not; a revoked
    /// grant stops being found) lives in
    /// `waddle-server::extension_job_outbox`'s tests, alongside the enqueue
    /// gate that is this method's real caller — see that module's
    /// `grant_derived_enqueue_gate` test group.
    pub fn durable_job_handler(&self, kind: &JobKind) -> Option<Arc<WasmExtensionActor>> {
        self.actors
            .iter()
            .find(|actor| {
                let manifest = actor.manifest();
                manifest.declares_capability(ExtensionCapability::DurableJob)
                    && actor.has_grant(ExtensionCapability::DurableJob)
                    && manifest.declares_job_kind(kind)
            })
            .cloned()
    }

    /// Convenience wrapper over [`Self::durable_job_handler`] for callers
    /// (the enqueue gate) that only need to know which plugin, if any,
    /// currently holds the grant — not the loaded actor itself.
    pub fn durable_job_grant_holder(&self, kind: &JobKind) -> Option<PluginId> {
        self.durable_job_handler(kind)
            .map(|actor| actor.manifest().id)
    }
}
