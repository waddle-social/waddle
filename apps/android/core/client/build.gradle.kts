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

    // Shared fakes (FakeClientFactory, in-memory stores, record builders)
    // consumed by :app unit and instrumentation tests.
    testFixtures {
        enable = true
    }

    lint {
        abortOnError = true
        warningsAsErrors = true
        // Version-bump nags churn weekly and carry no correctness signal;
        // dependency updates are deliberate, reviewed changes here.
        disable += setOf("AndroidGradlePluginVersion", "GradleDependency", "NewerVersionAvailable")
    }
}

dependencies {
    // The aar artifact is mandatory: it ships the JNA native dispatcher
    // .so that the generated UniFFI bindings load. The plain jar would
    // crash at runtime.
    api(libs.jna) {
        artifact {
            type = "aar"
        }
    }
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
