#!/usr/bin/env python3
"""Exercise the actual host cdylib's request, timeline, lifetime and buffer ownership ABI."""
import argparse
import ctypes
import json
import os
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument('library', nargs='?', default=os.environ.get('WADDLE_FLUTTER_RUNTIME_LIBRARY'))
args = parser.parse_args()
if not args.library:
    parser.error('supply the host library or WADDLE_FLUTTER_RUNTIME_LIBRARY')
lib = ctypes.CDLL(str(Path(args.library).resolve()))
lib.waddle_flutter_create.argtypes = []
lib.waddle_flutter_create.restype = ctypes.c_uint64
lib.waddle_flutter_submit.argtypes = [ctypes.c_uint64, ctypes.c_char_p]
lib.waddle_flutter_submit.restype = ctypes.c_uint64
lib.waddle_flutter_poll.argtypes = [ctypes.c_uint64]
lib.waddle_flutter_poll.restype = ctypes.c_void_p
lib.waddle_flutter_free_json.argtypes = [ctypes.c_void_p]
lib.waddle_flutter_destroy.argtypes = [ctypes.c_uint64]


def poll(handle):
    pointer = lib.waddle_flutter_poll(handle)
    assert pointer, 'poll must allocate an owned buffer'
    try:
        return json.loads(ctypes.string_at(pointer).decode())
    finally:
        lib.waddle_flutter_free_json(pointer)


for _ in range(16):
    handle = lib.waddle_flutter_create()
    assert handle > 0
    snapshot = {'method': 'timeline_snapshot', 'args': {'conversation_jid': 'room@conference.example.test'}}
    request = lib.waddle_flutter_submit(handle, json.dumps(snapshot).encode())
    assert request > 0
    frames = poll(handle)
    assert frames[0]['request_id'] == request
    assert frames[0]['ok'] is True
    assert frames[0]['data']['items'] == []
    assert poll(handle) == []
    bad = lib.waddle_flutter_submit(handle, b'{"method":"send_raw_xml","args":{}}')
    assert bad > request
    assert poll(handle)[0]['error']['code'] == 'invalid_request'
    lib.waddle_flutter_destroy(handle)
    lib.waddle_flutter_destroy(handle)
    assert lib.waddle_flutter_submit(handle, b'{}') == 0
    assert poll(handle) == []
print('Native cdylib ABI smoke passed (16 independent handle/buffer cycles).')
