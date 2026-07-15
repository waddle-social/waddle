plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
    // Nav keys are @Serializable so the Navigation 3 back stack survives
    // process death via rememberNavBackStack.
    alias(libs.plugins.kotlin.serialization)
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

        // REST auth + session base URL; override per build with
        // `-PwaddleServerUrl=https://…`.
        val serverUrl = (project.findProperty("waddleServerUrl") as String?)
            ?: "https://xmpp.waddle.social"
        buildConfigField("String", "WADDLE_SERVER_URL", "\"$serverUrl\"")
    }

    buildFeatures {
        compose = true
        buildConfig = true
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
    implementation(libs.compose.material.icons.extended)
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.core.splashscreen)
    implementation(libs.androidx.activity.compose)
    implementation(libs.androidx.browser)
    implementation(libs.androidx.lifecycle.runtime.compose)
    implementation(libs.androidx.lifecycle.viewmodel.compose)
    implementation(libs.androidx.lifecycle.process)
    implementation(libs.androidx.navigation3.runtime)
    implementation(libs.androidx.navigation3.ui)
    implementation(libs.coil.compose)
    implementation(libs.coil.network.okhttp)
    implementation(libs.kotlinx.coroutines.android)
    implementation(libs.kotlinx.serialization.core)

    testImplementation(libs.junit)
    testImplementation(libs.kotlinx.coroutines.test)
    testImplementation(libs.turbine)
}
