package social.waddle.android.client

/** Typed outcomes for bootstrap-pair worker exits. */
internal enum class BootstrapExitDisposition {
    NotBootstrap,
    ExpectedTeardown,
    RecordedFailure,
    DuplicateFailure,
    SecondaryFailure,
}
