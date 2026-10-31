import org.gradle.api.artifacts.component.ModuleComponentIdentifier
import org.gradle.api.artifacts.result.ResolvedComponentResult
import org.gradle.api.artifacts.result.ResolvedDependencyResult

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
    // Nav keys are @Serializable so the Navigation 3 back stack survives
    // process death via rememberNavBackStack.
    alias(libs.plugins.kotlin.serialization)
    alias(libs.plugins.roborazzi)
}

/** The FCM project values the Play build is configured with. */
val FIREBASE_CONFIG = listOf(
    "WADDLE_FIREBASE_PROJECT_ID",
    "WADDLE_FIREBASE_APP_ID",
    "WADDLE_FIREBASE_API_KEY",
    "WADDLE_FIREBASE_SENDER_ID",
)

fun firebaseValue(name: String): String =
    (project.findProperty(name) as String?) ?: System.getenv(name).orEmpty()

android {
    namespace = "social.waddle.android"
    compileSdk = 37

    defaultConfig {
        applicationId = "social.waddle.android"
        minSdk = 34
        targetSdk = 36
        versionCode = (project.findProperty("versionCode") as String?)?.toInt() ?: 1
        versionName = (project.findProperty("versionName") as String?) ?: "0.1.0-dev"
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"

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

    // Two distributions of one app (same applicationId; the two are
    // signed differently, so switching channels means reinstalling):
    flavorDimensions += "distribution"
    productFlavors {
        // Obtainium (and F-Droid): UnifiedPush or a background connection,
        // and no Google code at all (verifyFossRuntimeClasspath).
        create("foss") {
            dimension = "distribution"
            isDefault = true
        }
        // Google Play: FCM push and no background connection service.
        // FCM is configured from these values (Gradle property or
        // environment variable), never google-services.json; without them
        // the build has no FCM, and playRelease refuses to build.
        create("play") {
            dimension = "distribution"
            for (name in FIREBASE_CONFIG) {
                buildConfigField("String", name, "\"${firebaseValue(name)}\"")
            }
        }
    }

    signingConfigs {
        // Shared, checked-in debug keystore (deliberately NOT a secret —
        // it may never sign release builds): every machine and CI run
        // produces identically-signed debug APKs, so `adb install -r`
        // upgrades in place instead of demanding an uninstall (which
        // wipes app data) whenever the APK came from a different builder.
        getByName("debug") {
            storeFile = rootProject.file("debug.keystore")
            storePassword = "android"
            keyAlias = "androiddebugkey"
            keyPassword = "android"
        }
    }

    lint {
        abortOnError = true
        warningsAsErrors = true
        checkDependencies = true
        // Version-bump nags churn weekly and carry no correctness signal;
        // dependency updates are deliberate, reviewed changes here.
        disable += setOf("AndroidGradlePluginVersion", "GradleDependency", "NewerVersionAvailable")
        // targetSdk upgrades are deliberate, tested changes, not nags.
        disable += "OldTargetApi"
        // The FOSS build holds a persistent XMPP connection in a foreground
        // service, the documented use of the battery-optimization
        // exemption; the Play manifest removes both.
        disable += "BatteryLife"
        // Release APKs are arm64-only by design (sideload target); x86_64
        // stays debug/emulator-only.
        disable += "ChromeOsAbiSupport"
    }

    testOptions {
        // Robolectric screenshot tests render real resources (fonts, the
        // icon) with native graphics.
        unitTests {
            isIncludeAndroidResources = true
            all { test ->
                test.systemProperty("robolectric.graphicsMode", "NATIVE")
                test.systemProperty("robolectric.pixelCopyRenderMode", "hardware")
            }
        }
        managedDevices {
            localDevices {
                // Headless automated-test device for CI (no GPU, low
                // memory): `./gradlew :app:atdApi34FossDebugAndroidTest`.
                // API 34 = minSdk; the aosp-atd image line trails the
                // newest platforms.
                create("atdApi34") {
                    device = "Pixel 8"
                    apiLevel = 34
                    systemImageSource = "aosp-atd"
                }
            }
        }
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
            // Store releases sign with the upload key from the environment
            // (never committed). Absent credentials leave the APK unsigned.
            val uploadKeystore = System.getenv("WADDLE_ANDROID_UPLOAD_KEYSTORE")
            if (uploadKeystore != null) {
                signingConfig = signingConfigs.create("upload") {
                    storeFile = file(uploadKeystore)
                    storePassword = System.getenv("WADDLE_ANDROID_UPLOAD_STORE_PASSWORD")
                    keyAlias = System.getenv("WADDLE_ANDROID_UPLOAD_KEY_ALIAS")
                    keyPassword = System.getenv("WADDLE_ANDROID_UPLOAD_KEY_PASSWORD")
                }
            }
        }
        // The sideloaded rolling build: release-optimised (R8, shrinking,
        // non-debuggable, mobile-profile Rust core) but signed with the
        // shared checked-in key so it installs in place over earlier
        // rolling and debug builds without wiping app data. The key is
        // public, so this APK proves nothing about its origin; store
        // distribution uses `release` and the upload key instead.
        create("preview") {
            initWith(getByName("release"))
            signingConfig = signingConfigs.getByName("debug")
            matchingFallbacks += "release"
        }
    }
}

androidComponents {
    // The Play build ships only as playRelease; the public preview key
    // must never sign an APK with Play's FCM configuration.
    beforeVariants(selector().withFlavor("distribution" to "play").withBuildType("preview")) {
        it.enable = false
    }
}

dependencies {
    implementation(project(":flutter"))
    implementation(project(":core:client"))
    // FCM for push, and nothing else from Google (see the verify tasks below).
    "playImplementation"(libs.firebase.messaging)

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
    // Installs the bundled Compose/AndroidX baseline profiles (and the
    // app's own, when generated) so release builds skip JIT warm-up.
    implementation(libs.androidx.profileinstaller)
    implementation(libs.unifiedpush.connector) {
        exclude(group = "com.google.crypto.tink", module = "tink")
    }
    implementation(libs.tink.android)
    implementation(libs.androidx.navigation3.runtime)
    implementation(libs.androidx.navigation3.ui)
    implementation(libs.androidx.lifecycle.viewmodel.navigation3)
    implementation(libs.androidx.adaptive.navigation3)
    implementation(libs.coil.compose)
    implementation(libs.coil.network.okhttp)
    implementation(libs.kotlinx.coroutines.android)
    implementation(libs.kotlinx.serialization.core)
    // A/V call media plane (WebRTC to the LiveKit SFU); signaling stays
    // XMPP-native through :core:client.
    implementation(libs.livekit.android)

    testImplementation(libs.junit)
    testImplementation(libs.kotlinx.coroutines.test)
    testImplementation(libs.turbine)
    testImplementation(testFixtures(project(":core:client")))
    testImplementation(composeBom)
    testImplementation(libs.robolectric)
    testImplementation(libs.roborazzi)
    testImplementation(libs.roborazzi.compose)
    testImplementation(libs.roborazzi.junit.rule)
    testImplementation(libs.compose.ui.test.junit4)
    testImplementation(libs.androidx.test.ext.junit)

    // Hosts ComponentActivity for createAndroidComposeRule in androidTest.
    debugImplementation(libs.compose.ui.test.manifest)

    androidTestImplementation(composeBom)
    androidTestImplementation(libs.androidx.test.runner)
    androidTestImplementation(libs.androidx.test.ext.junit)
    androidTestImplementation(libs.compose.ui.test.junit4)
    androidTestImplementation(libs.kotlinx.coroutines.test)
    androidTestImplementation(testFixtures(project(":core:client")))
}

// Golden screenshots live in the source tree so CI can verify them:
// `./gradlew :app:verifyRoborazziFossDebug`; re-record with
// `./gradlew :app:recordRoborazziFossDebug` after an intended visual change.
roborazzi {
    outputDir.set(file("src/test/screenshots"))
}

// A Play release without FCM would ship with no background delivery.
val checkPlayReleaseFirebaseConfig by tasks.registering {
    val missing = FIREBASE_CONFIG.filter { firebaseValue(it).isBlank() }
    doLast {
        if (missing.isNotEmpty()) {
            throw GradleException("playRelease needs FCM configuration: ${missing.joinToString()}")
        }
    }
}
tasks.matching { it.name == "prePlayReleaseBuild" }.configureEach { dependsOn(checkPlayReleaseFirebaseConfig) }

/**
 * Fails when a resolved runtime classpath contains a banned module: the
 * FOSS build has no Google code at all, and the Play build carries
 * FCM but no analytics, crash or performance reporting (the banned
 * module prefixes live in gradle/runtime-bans-*.txt).
 * Resolved, so transitive dependencies count too.
 */
abstract class VerifyRuntimeClasspath : DefaultTask() {
    @get:Input
    abstract val rootComponent: Property<ResolvedComponentResult>

    @get:Input
    abstract val bannedPrefixes: ListProperty<String>

    @TaskAction
    fun verify() {
        val modules = mutableSetOf<String>()
        val visited = mutableSetOf<ResolvedComponentResult>()
        val queue = ArrayDeque(listOf(rootComponent.get()))
        while (queue.isNotEmpty()) {
            val component = queue.removeFirst()
            if (!visited.add(component)) continue
            (component.id as? ModuleComponentIdentifier)?.let { modules += "${it.group}:${it.module}" }
            component.dependencies.filterIsInstance<ResolvedDependencyResult>().forEach { queue += it.selected }
        }
        val banned = modules.filter { module -> bannedPrefixes.get().any(module::startsWith) }.sorted()
        if (banned.isNotEmpty()) {
            throw GradleException("$name: banned modules on the runtime classpath: ${banned.joinToString()}")
        }
    }
}

fun bannedModules(flavor: String): List<String> =
    rootProject.file("gradle/runtime-bans-$flavor.txt").readLines()
        .map(String::trim)
        .filter { it.isNotEmpty() && !it.startsWith("#") }

val verifyFossRuntimeClasspath by tasks.registering(VerifyRuntimeClasspath::class) {
    rootComponent.set(
        configurations.named("fossReleaseRuntimeClasspath").flatMap { it.incoming.resolutionResult.rootComponent },
    )
    bannedPrefixes.set(bannedModules("foss"))
}

val verifyPlayRuntimeClasspath by tasks.registering(VerifyRuntimeClasspath::class) {
    rootComponent.set(
        configurations.named("playReleaseRuntimeClasspath").flatMap { it.incoming.resolutionResult.rootComponent },
    )
    bannedPrefixes.set(bannedModules("play"))
}
