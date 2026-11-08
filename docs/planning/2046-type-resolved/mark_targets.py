#!/usr/bin/env python3
"""Add temporary W2046 deprecation markers to a disposable source worktree.

This script deliberately refuses the checkout that contains it. Run it only
against a clean, disposable worktree at the pinned source commit; Cargo's JSON
compiler diagnostics then provide type-resolved use-site spans. The annotated
tree is evidence for the pass and must never be committed as serving code.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import os
import pathlib
import re
import subprocess
import sys


SOURCE_COMMIT = "3ae674c121c06b44934c915ecc0ef469dca54833"
MARKER = "W2046|"


def strip_code(text: str) -> str:
    """Blank comments and strings while preserving offsets and newlines."""
    out: list[str] = []
    i, n = 0, len(text)
    while i < n:
        if text.startswith("//", i):
            j = text.find("\n", i)
            j = n if j < 0 else j
            out.append(" " * (j - i))
            i = j
        elif text.startswith("/*", i):
            depth, j = 1, i + 2
            while j < n and depth:
                if text.startswith("/*", j):
                    depth += 1
                    j += 2
                elif text.startswith("*/", j):
                    depth -= 1
                    j += 2
                else:
                    j += 1
            out.append("".join(c if c == "\n" else " " for c in text[i:j]))
            i = j
        elif text[i] == "r" and re.match(r'r(#*)"', text[i:]):
            m = re.match(r'r(#*)"', text[i:])
            assert m is not None
            end_token = '"' + m.group(1)
            j = text.find(end_token, i + len(m.group(0)))
            j = n if j < 0 else j + len(end_token)
            out.append("".join(c if c == "\n" else " " for c in text[i:j]))
            i = j
        elif text[i] == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            j = min(n, j + 1)
            out.append("".join(c if c == "\n" else " " for c in text[i:j]))
            i = j
        elif text[i] == "'" and i + 2 < n and text[i + 2] == "'":
            out.append("   ")
            i += 3
        else:
            out.append(text[i])
            i += 1
    return "".join(out)


def item_end(code: str, start: int) -> int:
    depth, parens, i = 0, 0, start
    while i < len(code):
        c = code[i]
        if c in "([":
            parens += 1
        elif c in ")]":
            parens -= 1
        elif c == "{" and parens == 0:
            depth = 1
            i += 1
            while i < len(code) and depth:
                if code[i] == "{":
                    depth += 1
                elif code[i] == "}":
                    depth -= 1
                i += 1
            return i
        elif c == ";" and parens == 0:
            return i + 1
        i += 1
    return len(code)


def line_start(text: str, offset: int) -> int:
    return text.rfind("\n", 0, offset) + 1


def add_target(targets: list[dict[str, str]], path: pathlib.Path, text: str, offset: int, tag: str, kind: str) -> None:
    start = line_start(text, offset)
    indent = re.match(r"[ \t]*", text[start:]).group(0)  # type: ignore[union-attr]
    line = text.count("\n", 0, offset) + 1
    column = offset - line_start(text, offset) + 1
    # Rust allows the same field or method name in multiple enum variants,
    # cfg-specific impls, and types. Make every diagnostic tag point to one
    # exact source declaration, even when its readable base label repeats.
    unique_tag = f"{tag}@L{line}C{column}"
    targets.append({"path": path.as_posix(), "offset": str(start), "line": str(line), "column": str(column), "tag": unique_tag, "kind": kind, "indent": indent})


def source_files(root: pathlib.Path, pattern: str) -> list[pathlib.Path]:
    base = root / pattern
    if base.is_file():
        return [base]
    return sorted(p for p in base.parent.glob(base.name) if p.is_file())


def read_source(path: pathlib.Path) -> str:
    with path.open(encoding="utf-8", newline="") as source:
        return source.read()


def add_struct_targets(root: pathlib.Path, targets: list[dict[str, str]], rel: str, name: str, tag: str, fields: bool = True, mark_type: bool = True) -> None:
    path = root / rel
    text = read_source(path)
    code = strip_code(text)
    m = re.search(r"\b(?:struct|enum)\s+" + re.escape(name) + r"\b", code)
    if not m:
        raise ValueError(f"missing {name} in {rel}")
    if mark_type:
        add_target(targets, pathlib.Path(rel), text, m.start(), f"{tag}::TYPE", "type")
    if fields:
        end = item_end(code, m.start())
        body = code[m.start():end]
        for fm in re.finditer(r"(?m)^\s*(?:(?:pub)(?:\([^)]*\))?\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*:(?!:)", body):
            field = fm.group(1)
            add_target(targets, pathlib.Path(rel), text, m.start() + fm.start(1), f"{tag}.{field}", "field")


def add_impl_targets(root: pathlib.Path, targets: list[dict[str, str]], rel: str, type_name: str, tag: str, methods: set[str] | None = None) -> None:
    path = root / rel
    text = read_source(path)
    code = strip_code(text)
    impl_re = re.compile(r"\bimpl(?:\s*<[^{}]*>)?\s+" + re.escape(type_name) + r"\b[^{}]*\{")
    for im in impl_re.finditer(code):
        header = code[im.start():code.find("{", im.start())]
        if re.search(r"\bfor\b", header):
            continue
        end = item_end(code, im.start())
        for fm in re.finditer(r"(?m)^\s*(?:(?:pub)(?:\([^)]*\))?\s+)?(?:(?:async|unsafe|const)\s+)*fn\s+([A-Za-z_][A-Za-z0-9_]*)", code[im.start():end]):
            name = fm.group(1)
            if methods is not None and name not in methods:
                continue
            offset = im.start() + fm.start(1)
            add_target(targets, pathlib.Path(rel), text, offset, f"{tag}::{name}", "method")


def add_free_targets(root: pathlib.Path, targets: list[dict[str, str]], rel: str, names: set[str], tag: str) -> None:
    path = root / rel
    text = read_source(path)
    code = strip_code(text)
    for name in sorted(names):
        matches = list(re.finditer(r"(?m)^\s*(?:(?:pub)(?:\([^)]*\))?\s+)?(?:(?:async|unsafe|const)\s+)*fn\s+" + re.escape(name) + r"\b", code))
        if not matches:
            raise ValueError(f"missing function {name} in {rel}")
        for m in matches:
            add_target(targets, pathlib.Path(rel), text, m.start(), f"{tag}::{name}", "function")


def add_named_type_family(root: pathlib.Path, targets: list[dict[str, str]], rel: str, name_pattern: str, tag: str) -> None:
    path = root / rel
    text = read_source(path)
    code = strip_code(text)
    name_re = re.compile(name_pattern)
    found = 0
    decl_re = re.compile(r"\b(struct|enum)\s+([A-Za-z_][A-Za-z0-9_]*)\b")
    for m in decl_re.finditer(code):
        declaration_kind, name = m.group(1), m.group(2)
        if not name_re.search(name):
            continue
        found += 1
        add_target(targets, pathlib.Path(rel), text, m.start(), f"{tag}::{name}::TYPE", "type")
        end = item_end(code, m.start())
        body = code[m.start():end]
        for fm in re.finditer(r"(?m)^\s*(?:(?:pub)(?:\([^)]*\))?\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*:(?!:)", body):
            add_target(targets, pathlib.Path(rel), text, m.start() + fm.start(1), f"{tag}::{name}.{fm.group(1)}", "field")
        if declaration_kind == "enum":
            opening = code.find("{", m.start(), end)
            depth = 1
            for line_match in re.finditer(r"(?m)^.*$", code[opening + 1:end]):
                line = line_match.group(0)
                if depth == 1:
                    variant = re.match(r"\s*([A-Z][A-Za-z0-9_]*)\s*(?:\(|\{|,|=)", line)
                    if variant:
                        add_target(targets, pathlib.Path(rel), text, opening + 1 + line_match.start() + variant.start(1), f"{tag}::{name}::{variant.group(1)}", "variant")
                depth += line.count("{") - line.count("}")
    if not found:
        raise ValueError(f"no type declarations matched {name_pattern} in {rel}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=pathlib.Path, required=True, help="clean disposable source worktree at the pinned commit")
    parser.add_argument("--manifest", type=pathlib.Path, required=True, help="TSV path outside server/crates in the disposable worktree")
    args = parser.parse_args()
    root = args.root.resolve()
    source_checkout = pathlib.Path(__file__).resolve().parents[3]
    if root == source_checkout:
        raise SystemExit("refusing to annotate the #2286 source checkout")
    head = subprocess.check_output(["git", "-C", str(root), "rev-parse", "HEAD"], text=True).strip()
    if head != SOURCE_COMMIT:
        raise SystemExit(f"expected disposable tree at {SOURCE_COMMIT}, found {head}")
    status = subprocess.check_output(["git", "-C", str(root), "status", "--short"], text=True)
    if status.strip():
        raise SystemExit("disposable marker worktree must be clean before annotation")

    targets: list[dict[str, str]] = []
    xm = "server/crates/waddle-xmpp/src/"
    sv = "server/crates/waddle-server/src/"
    cr = xm + "registry/connection_registry/"
    registry_files = [root / (xm + "registry/connection_registry.rs"), *sorted((root / cr).glob("*.rs"))]
    registry_rels = [p.relative_to(root).as_posix() for p in registry_files if "tests" not in p.stem]

    add_struct_targets(root, targets, xm + "registry/connection_registry.rs", "ConnectionRegistry", "CRF")
    add_struct_targets(root, targets, cr + "outbound.rs", "ConnectionEntry", "CEF")
    add_struct_targets(root, targets, cr + "outbound.rs", "OutboundStanza", "OBE")
    add_struct_targets(root, targets, cr + "outbound.rs", "OutboundWriteAcceptance", "OBACK", fields=False)
    add_struct_targets(root, targets, sv + "server/routes/websocket/state.rs", "WsConnState", "WS")
    for rel in registry_rels:
        add_impl_targets(root, targets, rel, "ConnectionRegistry", "CR")
        add_impl_targets(root, targets, rel, "ConnectionEntry", "CE")
        add_impl_targets(root, targets, rel, "OutboundStanza", "OBE")
        add_impl_targets(root, targets, rel, "OutboundWriteAcceptance", "OBACK")

    endpoint_file = cr + "endpoint.rs"
    for type_name in [
        "SocketOutboundEndpoint",
        "SocketOutboundReceiver",
        "ActorSocketOutboundEndpoint",
        "BindingAdmission",
        "BindingOutboundEndpoint",
        "AdmittedOutboundEndpoint",
        "EndpointPermit",
        "EndpointEnqueueOutcome",
        "OutboundQueue",
    ]:
        tag = f"EP::{type_name}"
        add_struct_targets(root, targets, endpoint_file, type_name, tag)
        add_impl_targets(root, targets, endpoint_file, type_name, tag)
    for type_name in ["ActorOutboundCustody", "ActorOutboundReceiver", "ActorOutboundId"]:
        tag = f"ACTOROUT::{type_name}"
        add_struct_targets(root, targets, cr + "actor_outbound.rs", type_name, tag)
        add_impl_targets(root, targets, cr + "actor_outbound.rs", type_name, tag)

    user_actor_files = [xm + "registry/user_actor.rs", xm + "registry/user_actor/implementation.rs", xm + "registry/user_actor/delivery.rs"]
    add_struct_targets(root, targets, xm + "registry/user_actor/implementation.rs", "UserActor", "UAF")
    for rel in user_actor_files:
        add_impl_targets(root, targets, rel, "UserActor", "UA")
        # Public actor messages are separately marked to capture typed senders.
        add_named_type_family(root, targets, rel, r"^(?:Register|Unregister|TrySend|GetConnection|.*Resource.*)$", "UAMSG")

    add_struct_targets(root, targets, xm + "registry/user_registry.rs", "UserRegistryActor", "URF")
    add_struct_targets(root, targets, xm + "registry/user_registry.rs", "UserEntry", "URE")
    add_impl_targets(root, targets, xm + "registry/user_registry.rs", "UserRegistryActor", "UR")
    add_named_type_family(root, targets, xm + "registry/user_registry.rs", r"(?:UserResource|Resource|Claim)", "URMSG")

    add_free_targets(root, targets, sv + "server/dual_registration.rs", {"mirror_register", "mirror_register_outcome", "mirror_unregister"}, "DUAL")
    add_free_targets(root, targets, sv + "server/routes/interpret/room_dispatch.rs", {"dispatch_to_room", "capture_delivered_remote_room_route"}, "ROOM")
    add_free_targets(root, targets, sv + "server/routes/interpret/relay_plan.rs", {"plan_muc_for_relay"}, "ROOM")
    add_impl_targets(root, targets, sv + "server/routes/interpret/room_routing.rs", "RoomMessageRoute", "ROOM")

    bridge_files = sorted((root / (sv + "clustering/route_bridge")).rglob("*.rs"))
    for path in bridge_files:
        if "tests" in path.stem or "test" in path.parts:
            continue
        add_impl_targets(root, targets, path.relative_to(root).as_posix(), "OrderedRelayDeliveryBridge", "BRIDGE")

    add_named_type_family(root, targets, sv + "clustering/ordered_relay.rs", r"^OrderedRelayMucProxyKind$", "MUCKIND")
    add_named_type_family(root, targets, sv + "clustering/relay/implementation.rs", r"(?:RemoteResource|RemoteUserResource)", "RRE")
    add_named_type_family(root, targets, sv + "clustering/relay/remote_resource_compat/wire.rs", r"(?:RouteV9|FrameV3|LiveRoute|LiveFrame)", "RRECOMPAT")
    add_named_type_family(root, targets, sv + "clustering/relay/resource_identity_wire.rs", r"(?:RemoteResource|RemoteUserResource)", "RRESERVED")
    add_named_type_family(root, targets, sv + "clustering/route_bridge/types.rs", r"^RemoteResource", "RRETYPE")
    add_impl_targets(root, targets, sv + "clustering/relay/implementation.rs", "RelayActor", "RELAY")
    add_free_targets(root, targets, xm + "ownership/claims.rs", {"current_claim", "current_claim_after_pending_writes"}, "CLAIM")
    add_struct_targets(root, targets, sv + "server/routes/websocket/connection.rs", "LiveSocketState", "LIVEWS")
    add_impl_targets(root, targets, sv + "server/routes/websocket/connection.rs", "LiveSocketState", "LIVEWS")
    add_free_targets(root, targets, sv + "server/routes/websocket/outbound.rs", {"handle_outbound_stanza"}, "WSOUT")
    add_free_targets(root, targets, sv + "server/routes/websocket/registration.rs", {"register_bound_connection_after_frame", "register_bound_connection_after_frame_with_admission", "rollback_registered_connection"}, "WSREG")
    add_free_targets(root, targets, sv + "room_effect_outbox/drain.rs", {"queue_local", "await_acks"}, "ROOMEFFECT")

    # The source identity and every targeted file hash make the evidence
    # reproducible and let the parser distinguish stale diagnostics.
    by_file: dict[str, list[dict[str, str]]] = {}
    for target in targets:
        by_file.setdefault(target["path"], []).append(target)
    tag_targets: dict[str, set[tuple[str, str, str]]] = {}
    for target in targets:
        identity = (target["path"], target["line"], target["column"], target["kind"])
        tag_targets.setdefault(target["tag"], set()).add(identity)
    collisions = {tag: identities for tag, identities in tag_targets.items() if len(identities) != 1}
    duplicates = len(targets) - len(tag_targets)
    if collisions or duplicates:
        raise SystemExit(f"marker tags must map one-to-one to declarations: collisions={len(collisions)} duplicate_tags={duplicates}")

    manifest_rows: list[dict[str, str]] = []
    for rel, entries in sorted(by_file.items()):
        path = root / rel
        original = read_source(path)
        if MARKER in original:
            raise SystemExit(f"marker already present in {rel}")
        digest = hashlib.sha256(original.encode()).hexdigest()
        insertions: dict[int, list[dict[str, str]]] = {}
        for item in entries:
            insertions.setdefault(int(item["offset"]), []).append(item)
        updated = original
        for offset in sorted(insertions, reverse=True):
            items = sorted(insertions[offset], key=lambda x: x["tag"])
            tags = sorted({item["tag"] for item in items})
            notes = ";".join(MARKER + tag for tag in tags)
            marker = f'{items[0]["indent"]}#[deprecated(note = "{notes}")]\n'
            updated = updated[:offset] + marker + updated[offset:]
            for item in items:
                manifest_rows.append({"file": rel, "line": item["line"], "column": item["column"], "tag": item["tag"], "kind": item["kind"], "sha256_before": digest})
        path.write_bytes(updated.encode("utf-8"))

    args.manifest.parent.mkdir(parents=True, exist_ok=True)
    with args.manifest.open("w", encoding="utf-8", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=["file", "line", "column", "tag", "kind", "sha256_before"], delimiter="\t", lineterminator="\n")
        writer.writeheader()
        writer.writerows(sorted(manifest_rows, key=lambda r: (r["file"], int(r["line"]), r["tag"])))
    print(json.dumps({"source_commit": head, "targets": len(manifest_rows), "files": len(by_file), "manifest": str(args.manifest.resolve())}, sort_keys=True))


if __name__ == "__main__":
    main()
