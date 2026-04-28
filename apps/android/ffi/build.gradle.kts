import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import org.jetbrains.kotlin.gradle.dsl.ExplicitApiMode

plugins {
    alias(libs.plugins.androidLibrary)
    alias(libs.plugins.ktlint)
}

android {
    namespace = "social.waddle.android.ffi"
    compileSdk = 36
    ndkVersion = "27.2.12479018"

    defaultConfig {
        minSdk = 36
        consumerProguardFiles("consumer-rules.pro")
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_21
        targetCompatibility = JavaVersion.VERSION_21
    }
}

kotlin {
    jvmToolchain(25)
    explicitApi = ExplicitApiMode.Strict
    compilerOptions {
        jvmTarget.set(JvmTarget.JVM_21)
    }
}

dependencies {
    api(libs.kotlinx.coroutines.core)
    api(libs.jna) { artifact { type = "aar" } }

    testImplementation(libs.junit)
    testImplementation(libs.kotlinx.coroutines.test)
    testImplementation(libs.turbine)
}

// Fail fast if the operator forgot to run scripts/build-android-bindings.sh
tasks.register("verifyFfiArtifacts") {
    group = "verification"
    description = "Ensures the Rust .so and uniffi-generated Kotlin bindings exist."

    val expectedSo = layout.projectDirectory
        .file("src/main/jniLibs/arm64-v8a/libwaddle_xmpp_client_ffi.so")
    val expectedKotlin = layout.projectDirectory
        .dir("src/main/kotlin/uniffi/waddle_xmpp_client")

    doLast {
        if (!expectedSo.asFile.exists()) {
            throw GradleException(
                "Missing $expectedSo\n" +
                    "Run `bash scripts/build-android-bindings.sh` from the repo root first.",
            )
        }
        if (!expectedKotlin.asFile.exists()) {
            throw GradleException(
                "Missing $expectedKotlin\n" +
                    "Run `bash scripts/build-android-bindings.sh` from the repo root first.",
            )
        }
    }
}

tasks.named("preBuild") {
    dependsOn("verifyFfiArtifacts")
}
