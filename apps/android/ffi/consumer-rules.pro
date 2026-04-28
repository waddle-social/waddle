# Keep all uniffi-generated bindings — they're called via JNI and reflection.
-keep class uniffi.** { *; }
-keep class social.waddle.android.ffi.** { *; }
