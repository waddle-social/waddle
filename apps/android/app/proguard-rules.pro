# Keep uniffi/JNA glue
-keep class uniffi.** { *; }
-keep class com.sun.jna.** { *; }
-keep class social.waddle.android.ffi.** { *; }

# Compose
-keep class androidx.compose.** { *; }
-dontwarn androidx.compose.**
