plugins {
    id("com.android.application")
    // The Flutter Gradle Plugin must be applied after the Android and Kotlin Gradle plugins.
    id("dev.flutter.flutter-gradle-plugin")
}

val firebaseNames = listOf("PROJECT_ID", "APP_ID", "API_KEY", "SENDER_ID")
val firebaseSettings = firebaseNames.associateWith { name ->
    val key = "WADDLE_FIREBASE_$name"
    providers.gradleProperty(key).orElse(providers.environmentVariable(key)).orNull?.trim().orEmpty()
}
val firebaseConfigured = firebaseSettings.values.any { it.isNotEmpty() }
require(!firebaseConfigured || firebaseSettings.values.all { it.isNotEmpty() }) {
    "Configure all four WADDLE_FIREBASE settings for this Android package or leave them all unset"
}

val runtimeAbis = mapOf("android-arm64" to "arm64-v8a", "android-x64" to "x86_64")
val targetPlatforms = providers.gradleProperty("target-platform").orNull?.split(",") ?: runtimeAbis.keys
val targetAbis = targetPlatforms.mapNotNull(runtimeAbis::get)
require(targetAbis.isNotEmpty()) { "Waddle supports Android arm64 and x86_64" }
val sdkDirectory = androidComponents.sdkComponents.sdkDirectory
val buildWaddleRuntime by tasks.registering(Exec::class) {
    workingDir(rootDir.resolve("../../.."))
    commandLine("bash", "scripts/build-flutter-rust.sh", "--android")
    doFirst { environment("ANDROID_HOME", sdkDirectory.get().asFile.absolutePath) }
}

tasks.named("preBuild") { dependsOn(buildWaddleRuntime) }

android {
    namespace = "social.waddle.flutter"
    compileSdk = flutter.compileSdkVersion
    ndkVersion = "28.2.13676358"

    compileOptions {
        isCoreLibraryDesugaringEnabled = true
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures { resValues = firebaseConfigured }
    sourceSets.getByName("main") {
        java.srcDir(if (firebaseConfigured) "src/fcm/java" else "src/noFcm/java")
    }
    if (firebaseConfigured) sourceSets.getByName("androidTest").java.srcDir("src/fcmTest/java")
    defaultConfig {
        if (firebaseConfigured) {
            testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
            mapOf("google_app_id" to "APP_ID", "google_api_key" to "API_KEY",
                "gcm_defaultSenderId" to "SENDER_ID", "project_id" to "PROJECT_ID").forEach { (resource, setting) ->
                resValue("string", resource, firebaseSettings.getValue(setting))
            }
        }
        manifestPlaceholders["fcmServiceEnabled"] = firebaseConfigured
        applicationId = "social.waddle.flutter"
        minSdk = 26
        targetSdk = flutter.targetSdkVersion
        versionCode = flutter.versionCode
        versionName = flutter.versionName
    }

    // Public preview key: stable APK upgrades across CI runs; never a release key.
    signingConfigs {
        getByName("debug") {
            storeFile = file("../preview.keystore")
            storePassword = "android"
            keyAlias = "waddle-flutter-preview"
            keyPassword = "android"
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
        }
    }

}

// Package the intersection of Flutter engines and Rust runtimes for this build.
androidComponents.finalizeDsl { extension ->
    val split = extension.splits.abi
    if (split.isEnable) {
        split.reset()
        split.include(*targetAbis.toTypedArray())
    } else {
        extension.defaultConfig.ndk.abiFilters.apply {
            clear()
            addAll(targetAbis)
        }
    }
}

// UnifiedPush uses desktop Tink; secure storage needs its Android implementation.
// Both artifacts contain the same classes, so the APK must package only Android Tink.
configurations.configureEach {
    resolutionStrategy.dependencySubstitution {
        substitute(module("com.google.crypto.tink:tink"))
            .using(module("com.google.crypto.tink:tink-android:1.23.0"))
            .because("Use one Android Tink implementation for push and secure storage")
    }
}

dependencies {
    // The story recorder draws the active flutter_webrtc track onto an Android recording surface.
    compileOnly("io.github.webrtc-sdk:android:150.7871.04")
    if (firebaseConfigured) {
        // Match the runner on Flutter integration_test's app runtime classpath.
        androidTestImplementation("androidx.test:runner:1.3.0")
        implementation("com.google.firebase:firebase-messaging:25.1.3")
        implementation("org.unifiedpush.android:connector:3.3.5")
        implementation("com.google.crypto.tink:tink-android:1.23.0")
    }
    coreLibraryDesugaring("com.android.tools:desugar_jdk_libs:2.1.5")
    testImplementation("junit:junit:4.13.2")
}

flutter {
    source = "../.."
}
