import org.jetbrains.kotlin.gradle.dsl.ExplicitApiMode
import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
	alias(libs.plugins.androidLibrary)
	alias(libs.plugins.kotlinSerialization)
	alias(libs.plugins.ktlint)
}

android {
	namespace = "social.waddle.android.domain"
	compileSdk = 36

	defaultConfig {
		minSdk = 36
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
	api(project(":ffi"))
	api(libs.kotlinx.coroutines.core)
	api(libs.kotlinx.serialization.json)

	testImplementation(libs.junit)
	testImplementation(libs.mockk)
	testImplementation(libs.kotlinx.coroutines.test)
	testImplementation(libs.turbine)
}
