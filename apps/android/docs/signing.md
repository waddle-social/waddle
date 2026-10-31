# Android signing

Waddle uses separate signing keys for rolling preview APKs and Play releases.

## Rolling preview APKs

After the `default` CI pipeline validates a push to `main`, a separate
publisher workflow signs the FOSS preview APK with repository Actions secrets
`WADDLE_ANDROID_PREVIEW_KEYSTORE_B64` and
`WADDLE_ANDROID_PREVIEW_KEYSTORE_PASSWORD`. It checks out the exact validated
commit and exposes the secrets only to the `publishApk` task. The keystore is
decoded into the runner's temporary directory for the build, then removed; it
is not stored in the repository or attached to the release. The validation
workflow does not receive either value.

Pull request builds and local builds use the machine's Android debug keystore.
They do not receive the preview signing secret, so a locally built APK and a CI
preview APK may not install over each other without uninstalling first.

The preview signing certificate has changed from the previously checked-in
debug keystore. Android requires an installed app and its update to have the
same signing certificate, so users upgrading from an older preview must
uninstall it and install the new APK once. Uninstalling removes local app data.
After that reinstall, new CI preview builds can update in place.

## Play releases

The preview key is not a Play upload key. Play release signing uses the
separate `WADDLE_ANDROID_UPLOAD_*` environment variables documented in the
project README.
