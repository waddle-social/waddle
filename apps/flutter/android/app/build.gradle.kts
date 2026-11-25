plugins {
    id("com.android.application")
    // The Flutter Gradle Plugin must be applied after the Android and Kotlin Gradle plugins.
    id("dev.flutter.flutter-gradle-plugin")
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

    defaultConfig {
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
    coreLibraryDesugaring("com.android.tools:desugar_jdk_libs:2.1.5")
    testImplementation("junit:junit:4.13.2")
}

flutter {
    source = "../.."
}
