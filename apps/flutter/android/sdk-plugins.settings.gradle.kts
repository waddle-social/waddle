// Nix provides Flutter's SDK packages through an immutable store. integration_test
// is the SDK's native Android plugin, so Gradle must configure a local copy rather
// than use its read-only source directory as a project. Its build only references
// files inside android/; the SDK and generated Flutter package metadata stay intact.
findProject(":integration_test")?.let { plugin ->
    val source = plugin.projectDir
    check(source.isDirectory) { "Flutter integration_test Android sources missing: $source" }
    val destination = rootDir.parentFile.resolve(".dart_tool/android-sdk-plugins/integration_test")
    check(source.canonicalFile != destination.canonicalFile) { "SDK plugin source must be separate from its build copy" }
    if (destination.exists()) {
        check(destination.deleteRecursively()) { "Could not replace local SDK plugin: $destination" }
    }
    check(source.copyRecursively(destination, overwrite = true)) { "Could not copy SDK plugin: $source" }
    plugin.projectDir = destination
}
