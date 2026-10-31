"""Offline fixture decoder, not a live stream supervisor or canary command."""

import sys

from frame import FRAME_BYTES, InvalidFrame, decode_frame

try:
    decode_frame(sys.stdin.buffer.read(FRAME_BYTES + 1))
except InvalidFrame:
    sys.exit("invalid probe frame")
print("synthetic frame decoded; host confinement remains unverified")
