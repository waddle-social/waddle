pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode = RepositoriesMode.FAIL_ON_PROJECT_REPOS
    repositories {
        google()
        mavenCentral()
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
