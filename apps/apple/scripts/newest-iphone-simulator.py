#!/usr/bin/env python3
"""Prints the UDID of an available iPhone simulator on the newest iOS runtime.

The app tests render snapshots at a fixed scale, so the iPhone model does
not matter; the runtime does, because the goldens were recorded on iOS 27.
"""
import json
import re
import subprocess
import sys

devices = json.loads(
    subprocess.run(
        ["xcrun", "simctl", "list", "devices", "available", "--json"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
)["devices"]


def version(runtime: str) -> list[int]:
    # com.apple.CoreSimulator.SimRuntime.iOS-27-2 -> [27, 2]
    return [int(part) for part in re.findall(r"\d+", runtime.rsplit(".", 1)[-1])]


for runtime in sorted((r for r in devices if ".iOS-" in r), key=version, reverse=True):
    for device in devices[runtime]:
        if "iPhone" in device["deviceTypeIdentifier"]:
            print(device["udid"])
            sys.exit(0)

sys.exit("No available iPhone simulator")
