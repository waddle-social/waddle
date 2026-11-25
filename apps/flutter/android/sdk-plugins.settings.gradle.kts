// Gradle 9 requires a writable project for SDK-supplied integration_test.
// The Flutter WebRTC Android copy also forwards MediaProjection revocation.
fun localAndroidPlugin(name: String, configure: (File, File) -> Unit = { _, _ -> }) {
    val plugin = findProject(":$name") ?: return
    val source = plugin.projectDir
    check(source.isDirectory) { "Flutter $name Android sources missing: $source" }
    val destination = rootDir.parentFile.resolve(".dart_tool/android-sdk-plugins/$name")
    check(source.canonicalFile != destination.canonicalFile) { "Plugin source must be separate from its build copy" }
    if (destination.exists()) {
        check(destination.deleteRecursively()) { "Could not replace local plugin: $destination" }
    }
    check(source.copyRecursively(destination, overwrite = true)) { "Could not copy plugin: $source" }
    configure(source, destination)
    plugin.projectDir = destination
}

localAndroidPlugin("integration_test")
localAndroidPlugin("flutter_webrtc") { source, destination ->
    val version = source.parentFile.resolve("pubspec.yaml").useLines { lines ->
        lines.first { it.startsWith("version:") }.substringAfter(':').trim()
    }
    check(version == "1.6.3") { "Review the MediaProjection stop callback for flutter_webrtc $version" }
    val patch = rootDir.resolve("flutter-webrtc-1.6.3-projection-stop.patch")
    val process = ProcessBuilder("git", "apply", patch.absolutePath).directory(destination)
    process.environment()["GIT_CEILING_DIRECTORIES"] = destination.parentFile.canonicalPath
    check(process.inheritIO().start().waitFor() == 0) { "Could not patch flutter_webrtc's screen-capture callback" }
}
