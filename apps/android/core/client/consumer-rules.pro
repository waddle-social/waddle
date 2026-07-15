# JNA is reflection/JNI based; R8 (especially fullMode) strips what it
# cannot see. Pattern follows mozilla/application-services'
# proguard-rules-consumer-jna.pro.
-dontwarn java.awt.*
-keep class com.sun.jna.** { *; }
-keep class * extends com.sun.jna.** { *; }
-keepclassmembers class * extends com.sun.jna.** { public *; }

# UniFFI-generated FFI structs (RustBuffer/ForeignBytes/UniffiLib and the
# callback vtables) are resolved reflectively by JNA.
-keep class social.waddle.client.ffi.** { *; }

-keepattributes RuntimeVisibleAnnotations,RuntimeInvisibleAnnotations,Signature
