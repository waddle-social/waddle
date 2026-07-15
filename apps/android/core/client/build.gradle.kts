plugins {
    alias(libs.plugins.android.library)
    alias(libs.plugins.kotlin.serialization)
}

android {
    namespace = "social.waddle.client"
    compileSdk = 37
    // Must match scripts/setup-android-sdk.sh and scripts/build-android-rust.sh.
    ndkVersion = "28.2.13676358"

    defaultConfig {
        minSdk = 34
        consumerProguardFiles("consumer-rules.pro")
    }
}

dependencies {
    // @aar is mandatory: it ships the JNA native dispatcher .so that the
    // generated UniFFI bindings load. The plain jar would crash at runtime.
    api("net.java.dev.jna:jna:${libs.versions.jna.get()}@aar")
    // UniFFI-generated suspend functions poll through kotlinx-coroutines.
    api(libs.kotlinx.coroutines.core)
    // The android=true bindings annotate the SystemCleaner path with
    // @RequiresApi.
    implementation(libs.androidx.annotation)

    // Bridge layer: REST auth client, DataStore prefs, JSON snapshots.
    // `api` where the type appears in a public constructor/return signature.
    api(libs.okhttp)
    api(libs.kotlinx.serialization.json)
    api(libs.androidx.datastore.preferences)

    testImplementation(libs.junit)
    testImplementation(libs.kotlinx.coroutines.test)
    testImplementation(libs.turbine)
    testImplementation(libs.okhttp.mockwebserver3)
}
