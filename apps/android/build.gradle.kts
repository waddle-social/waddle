plugins {
    alias(libs.plugins.android.application) apply false
    alias(libs.plugins.android.library) apply false
    alias(libs.plugins.kotlin.compose) apply false
    alias(libs.plugins.kotlin.serialization) apply false
    alias(libs.plugins.detekt)
}

// Static analysis mirroring the repo's clippy `-D warnings` posture:
// default rules + formatting (ktlint), no baseline files — findings are
// fixed, or the rule deviation is justified in detekt.yml. One root task
// scans both modules (no type resolution, so no per-module classpath is
// needed); the generated UniFFI bindings are excluded as generated code,
// not as a suppressed finding.
detekt {
    buildUponDefaultConfig = true
    config.setFrom(files("detekt.yml"))
    source.setFrom(files("app/src", "core/client/src"))
}

tasks.withType<io.gitlab.arturbosch.detekt.Detekt>().configureEach {
    exclude("**/social/waddle/client/ffi/**")
}

dependencies {
    detektPlugins(libs.detekt.formatting)
}
