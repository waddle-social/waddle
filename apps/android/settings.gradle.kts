pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    // The Flutter Gradle plugin declares its engine repository on projects.
    // Keep resolution centralized and supply the same repository here.
    repositoriesMode = RepositoriesMode.PREFER_SETTINGS
    repositories {
        google()
        mavenCentral()
        maven("https://storage.googleapis.com/download.flutter.io") {
            content { includeGroup("io.flutter") }
        }
        // livekit-android depends on the davidliu audioswitch fork,
        // published only on JitPack. Scoped so nothing else resolves
        // from there.
        maven("https://jitpack.io") {
            content {
                includeGroup("com.github.davidliu")
            }
        }
    }
}

rootProject.name = "waddle-android"

include(":app")
include(":core:client")

// Flutter owns the UI; the Android app remains the installable host so its
// application id, services, push flavours, signing and session stores survive.
// Flutter generates this loader (and its SDK path) during `flutter pub get`.
val flutterLoader = file("flutter/.android/include_flutter.groovy")
check(flutterLoader.isFile) {
    "Flutter module metadata is missing. Install Flutter, then run " +
        "`bash scripts/setup-android-flutter.sh` from the repository root."
}
apply(from = flutterLoader)
