#!/usr/bin/env python3
"""Drive Android's projection consent and revocation in the disposable API 35 ATD."""

import re
import subprocess
import sys
import time
from xml.etree import ElementTree


def main(adb: str, serial: str) -> None:
    def shell(*args: str) -> str:
        return subprocess.check_output(
            [adb, "-s", serial, "shell", *args], text=True, timeout=15,
        )

    # This ATD supplies a small system UI with a single positive consent button.
    # Never accept an unrelated permission dialog or send input to a physical device.
    if (not serial.startswith("emulator-")
            or shell("getprop", "ro.build.version.sdk").strip() != "35"
            or shell("getprop", "ro.kernel.qemu").strip() != "1"):
        raise RuntimeError("Expected the disposable API 35 emulator")

    deadline = time.monotonic() + 580
    while time.monotonic() < deadline:
        if "MediaProjectionPermissionActivity" in shell("dumpsys", "window", "windows"):
            shell("uiautomator", "dump", "/sdcard/waddle-projection-dialog.xml")
            xml = shell("cat", "/sdcard/waddle-projection-dialog.xml")
            dialog = ElementTree.fromstring(xml)
            button = dialog.find(".//node[@package='com.android.fakesystemapp'][@resource-id='android:id/button1'][@text='START NOW']")
            if button is not None and button.get("enabled") == "true":
                x1, y1, x2, y2 = map(int, re.findall(r"\d+", button.get("bounds", "")))
                shell("input", "tap", str((x1 + x2) // 2), str((y1 + y2) // 2))
                break
        time.sleep(1)
    else:
        raise RuntimeError("The Android screen-sharing consent prompt did not appear")

    for _ in range(100):
        marker = subprocess.run(
            [adb, "-s", serial, "exec-out", "run-as", "social.waddle.flutter", "cat", "cache/screen-capture.ready"],
            capture_output=True, timeout=5,
        )
        if marker.stdout == b"active" and "social.waddle.flutter" in shell("dumpsys", "media_projection"):
            break
        time.sleep(0.25)
    else:
        raise RuntimeError("Android did not register a projection for the test app")

    # API 35's IMediaProjectionManager.stopActiveProjection() is transaction 7.
    # The ATD omits the normal status-bar control that invokes this system method.
    result = shell("service", "call", "media_projection", "7")
    if "00000000" not in result or "ffffffff" in result.lower():
        raise RuntimeError(f"Android refused to stop the projection: {result}")
    for _ in range(100):
        projecting = "social.waddle.flutter" in shell("dumpsys", "media_projection")
        service = "ScreenCaptureService" in shell("dumpsys", "activity", "services", "social.waddle.flutter")
        if not projecting and not service:
            print("Android revoked projection and the app retired its foreground service.")
            return
        time.sleep(0.25)
    raise RuntimeError("The Android projection or its foreground service remained active")


if __name__ == "__main__":
    main(*sys.argv[1:])
