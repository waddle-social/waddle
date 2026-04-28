import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
	alias(libs.plugins.androidLibrary)
	alias(libs.plugins.ktlint)
}

android {
	namespace = "social.waddle.android.ffi"
	compileSdk = 36
	ndkVersion = "28.2.13676358"

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
	// Explicit API mode is disabled here because uniffi's generated bindings
	// do not declare visibility on every public surface. Domain and app
	// modules still enforce strict mode.
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

// Fail fast if the operator forgot to run scripts/build-android-bindings.sh.
// Compilation only needs the generated Kotlin; packaging needs the .so.
abstract class VerifyFfiArtifacts : DefaultTask() {
	@get:org.gradle.api.tasks.Internal
	abstract val target: org.gradle.api.file.RegularFileProperty

	@get:org.gradle.api.tasks.Internal
	abstract val targetDir: org.gradle.api.file.DirectoryProperty

	@get:org.gradle.api.tasks.Input
	abstract val displayPath: org.gradle.api.provider.Property<String>

	@get:org.gradle.api.tasks.Input
	abstract val checkKind: org.gradle.api.provider.Property<String>

	@org.gradle.api.tasks.TaskAction
	fun verify() {
		val path = displayPath.get()
		val resolved =
			when (checkKind.get()) {
				"directory" -> targetDir.get().asFile
				"file" -> target.get().asFile
				else -> error("unknown checkKind ${checkKind.get()}")
			}
		logger.lifecycle("verifyFfi: checkKind=${checkKind.get()} path=$path resolved=$resolved exists=${resolved.exists()}")
		if (!resolved.exists()) {
			throw org.gradle.api.GradleException(
				"Missing $path (resolved to $resolved)\n" +
					"Run `bash scripts/build-android-bindings.sh` from the repo root first.",
			)
		}
	}
}

tasks.register<VerifyFfiArtifacts>("verifyFfiBindings") {
	group = "verification"
	description = "Ensures the uniffi-generated Kotlin bindings exist (required for compilation)."
	displayPath.set("src/main/kotlin/uniffi/waddle_xmpp_client")
	checkKind.set("directory")
	targetDir.set(layout.projectDirectory.dir("src/main/kotlin/uniffi/waddle_xmpp_client"))
}

tasks.register<VerifyFfiArtifacts>("verifyFfiNativeLib") {
	group = "verification"
	description = "Ensures the per-ABI .so files exist (required for packaging)."
	displayPath.set("src/main/jniLibs/arm64-v8a/libwaddle_xmpp_client_ffi.so")
	checkKind.set("file")
	target.set(layout.projectDirectory.file("src/main/jniLibs/arm64-v8a/libwaddle_xmpp_client_ffi.so"))
}

tasks.named("preBuild") {
	dependsOn("verifyFfiBindings")
}

// Native lib only matters when JNI libs are actually packaged. Avoid
// attaching to merge*JniLibFolders directly — that drags lint variants
// into the .so requirement, and lint doesn't need them.
tasks
	.matching { it.name.startsWith("package") && it.name.endsWith("JniLibs") }
	.configureEach { dependsOn("verifyFfiNativeLib") }
