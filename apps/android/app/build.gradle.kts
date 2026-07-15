plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
}

android {
    namespace = "social.waddle.android"
    compileSdk = 37

    defaultConfig {
        applicationId = "social.waddle.android"
        minSdk = 34
        targetSdk = 36
        versionCode = (project.findProperty("versionCode") as String?)?.toInt() ?: 1
        versionName = (project.findProperty("versionName") as String?) ?: "0.1.0-dev"
    }

    buildFeatures {
        compose = true
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro",
            )
            // The sideloaded release APK only ever runs on arm64 hardware;
            // x86_64 stays debug-only for emulators and keeps LiveKit's
            // libwebrtc out of the shipped artifact twice.
            ndk {
                abiFilters += "arm64-v8a"
            }
        }
    }
}

dependencies {
    implementation(project(":core:client"))

    val composeBom = platform(libs.compose.bom)
    implementation(composeBom)
    implementation(libs.compose.ui)
    implementation(libs.compose.ui.tooling.preview)
    implementation(libs.compose.material3)
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.activity.compose)
    implementation(libs.androidx.lifecycle.runtime.compose)
    implementation(libs.kotlinx.coroutines.android)

    testImplementation(libs.junit)
}
