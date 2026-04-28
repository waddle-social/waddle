import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
	alias(libs.plugins.androidApplication)
	alias(libs.plugins.composeCompiler)
	alias(libs.plugins.kotlinSerialization)
	alias(libs.plugins.ktlint)
}

android {
	namespace = "social.waddle.android"
	compileSdk = 36

	defaultConfig {
		applicationId = "social.waddle.android"
		minSdk = 36
		targetSdk = 36
		versionCode = 1
		versionName = "0.1.0"

		testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
		vectorDrawables.useSupportLibrary = true
	}

	buildTypes {
		debug {
			isMinifyEnabled = false
			applicationIdSuffix = ".debug"
			versionNameSuffix = "-debug"
		}
		release {
			isMinifyEnabled = true
			isShrinkResources = true
			proguardFiles(
				getDefaultProguardFile("proguard-android-optimize.txt"),
				"proguard-rules.pro",
			)
			signingConfig = signingConfigs.getByName("debug")
		}
	}

	bundle {
		abi {
			enableSplit = true
		}
		density {
			enableSplit = true
		}
	}

	compileOptions {
		sourceCompatibility = JavaVersion.VERSION_21
		targetCompatibility = JavaVersion.VERSION_21
	}

	buildFeatures {
		compose = true
		buildConfig = true
	}

	packaging {
		resources {
			excludes += "/META-INF/{AL2.0,LGPL2.1}"
		}
	}
}

kotlin {
	jvmToolchain(25)
	compilerOptions {
		jvmTarget.set(JvmTarget.JVM_21)
	}
}

dependencies {
	implementation(project(":ffi"))
	implementation(project(":domain"))

	implementation(libs.kotlinx.coroutines.android)

	implementation(libs.androidx.activity.compose)
	implementation(libs.androidx.lifecycle.runtime.ktx)
	implementation(libs.androidx.lifecycle.viewmodel.compose)
	implementation(libs.androidx.lifecycle.process)
	implementation(libs.androidx.navigation.compose)
	implementation(libs.androidx.datastore.preferences)
	implementation(libs.androidx.browser)

	implementation(platform(libs.compose.bom))
	implementation(libs.compose.runtime)
	implementation(libs.compose.ui)
	implementation(libs.compose.ui.graphics)
	implementation(libs.compose.foundation)
	implementation(libs.compose.material3)
	implementation(libs.compose.material3.adaptive)
	implementation(libs.compose.material3.adaptive.navigation.suite)
	implementation(libs.compose.material.icons.extended)
	implementation(libs.compose.ui.tooling.preview)
	debugImplementation(libs.compose.ui.tooling)

	implementation(libs.koin.core)
	implementation(libs.koin.android)
	implementation(libs.koin.androidx.compose)

	implementation(libs.okhttp)
	implementation(libs.coil.compose)
	implementation(libs.coil.network.okhttp)

	testImplementation(libs.junit)
	testImplementation(libs.mockk)
	testImplementation(libs.kotlinx.coroutines.test)
	testImplementation(libs.turbine)
}
