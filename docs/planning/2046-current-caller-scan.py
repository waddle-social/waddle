#!/usr/bin/env python3
"""Generate a deliberately overinclusive textual caller index; no Rust type inference."""

import argparse
import collections
import os
import pathlib
import re
import subprocess
import sys
from urllib.parse import quote

# Keep the audit from creating a bytecode cache in the source tree.
sys.dont_write_bytecode = True

from connection_caller_regions import classify
from connection_caller_regions import item_extent as extent
from connection_caller_regions import strip_code as strip

xm = "server/crates/waddle-xmpp/src/registry/"
ws = "server/crates/waddle-server/src/server/routes/websocket/"
modulefiles = [
    pathlib.Path(xm + "connection_registry.rs"),
    *pathlib.Path(xm + "connection_registry").glob("*.rs"),
]
structfiles = {
    "Registry": (pathlib.Path(xm + "connection_registry.rs"), "ConnectionRegistry"),
    "Entry": (pathlib.Path(xm + "connection_registry/outbound.rs"), "ConnectionEntry"),
    "Socket": (pathlib.Path(ws + "state.rs"), "WsConnState"),
}
classes = {
    "Registry": {
        "bind_slots": "A/T",
        "connections": "I/T",
        "pending_subscription_stanzas": "A",
        "presence_states": "A/I",
        "last_activity": "A",
        "started_at": "T",
        "sm_stream_owners": "I",
    },
    "Entry": {
        "binding_id": "A",
        "connection_ref": "A",
        "binding_ref": "A",
        "binding_permit": "A/T",
        "local_io_relinquishment": "T",
        "archive_dispatch": "A",
        "hosting": "I/O",
        "resume_control": "T",
        "terminal_ordering_retry": "T",
        "endpoint": "T",
        "carbons_enabled": "A/I",
        "presence_available": "A/I",
        "presence_priority": "A/I",
        "roster_interested": "A/I",
        "blocklist_interested": "A/I",
        "offline_flushed": "A",
        "pending_subscribes_flushed": "A",
        "sm_stream_id": "I",
        "occupancy_session": "I",
        "force_detach_tx": "T",
        "force_detach_rx": "T",
        "write_interruption": "T",
    },
    "Socket": {
        "ingress_generation": "A",
        "connection_ref": "A",
        "local_io_relinquishment": "T",
        "binding_ref": "A",
        "phase": "A",
        "stream_open_handled": "T",
        "stream_open_from": "T",
        "stream_open_wire_state": "T",
        "authenticated_session": "A",
        "occupancy_session": "A",
        "sm_state": "A",
        "sm_ingress_fence": "A",
        "sm_inbound_completion": "A",
        "inbound_frame_terminal": "T",
        "ordered_relay_handoff_tx": "T",
        "carbons_enabled": "A/I",
        "roster_interested": "A/I",
        "blocklist_interested": "A/I",
        "presence_available": "I",
        "presence_show": "I",
        "presence_status": "I",
        "presence_priority": "I",
        "presence_payloads": "I",
        "pending_subscribes_flushed": "I",
        "pending_resume_stream_id": "A",
        "pending_resume_h": "A",
        "pending_resume_claim": "A",
        "pending_finalized_resume_outcome": "T",
        "registry_owner": "A",
        "socket_lifecycle": "T",
        "recovered_pending_endpoint": "T",
        "resume_endpoint_handoff_blocked": "T",
        "write_interruption": "T",
        "live_resume_control": "T",
        "suppress_sm_record_next_batch": "T",
        "pending_sm_enable_commit": "A",
        "deferred_inbound": "T",
        "sm_recovery_required": "A",
        "terminal_sm_recovery": "A",
        "pending_replay_completions": "A",
        "terminal_sm_recovery_dropped": "A",
        "terminal_sm_recovery_drop_warned": "T",
        "send_window_pause_deadline": "T",
        "send_window_last_request_acked": "T",
        "state_machine": "A",
        "keepalive_config": "T",
    },
}


def line_at(code, pos):
    return code.count("\n", 0, pos) + 1


def fields(path, ty):
    raw = path.read_text()
    code = strip(raw)
    match = re.search(r"\bstruct\s+" + ty + r"\s*\{", code)
    end = extent(code, match.start())
    block = code[match.end() : end]
    category = next(key for key, value in structfiles.items() if value[1] == ty)
    known_fields = classes[category]
    excluded_fields = {
        "Registry": {"bind_pending_probes"},
        "Entry": set(),
        "Socket": {
            "resume_binding_attempts",
            "pre_final_principal_recheck_test_hook",
            "post_sm_finalization_test_hook",
        },
    }[category]
    found = {}
    for hit in re.finditer(r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?(\w+)\s*:", block):
        name = hit.group(1)
        line = line_at(code, match.end() + hit.start(1))
        found[name] = (str(path), line)
    unclassified = set(found) - set(known_fields) - excluded_fields
    missing = set(known_fields) - set(found)
    if unclassified or missing:
        raise ValueError(
            f"{ty}: unclassified={sorted(unclassified)}, missing={sorted(missing)}"
        )
    return {name: location for name, location in found.items() if name in known_fields}


def methods(ty):
    result = {}
    for path in modulefiles:
        code = strip(path.read_text())
        for impl in re.finditer(r"\bimpl\s+" + ty + r"\s*\{", code):
            block = code[impl.end() : extent(code, impl.start())]
            for hit in re.finditer(
                r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+(\w+)", block
            ):
                name = hit.group(1)
                line = line_at(code, impl.end() + hit.start(1))
                if (
                    classify(str(path), line) == "prod"
                    and "_for_test" not in name
                    and name != "observe_next_pending_bind"
                ):
                    result[name] = (str(path), line)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--all", action="store_true", help="include every textual candidate link"
    )
    parser.add_argument(
        "--output", default="docs/planning/2046-current-caller-candidates.md"
    )
    args = parser.parse_args()

    members = {k: fields(*v) for k, v in structfiles.items()}
    funcs = {
        "Registry": methods("ConnectionRegistry"),
        "Entry": methods("ConnectionEntry"),
    }
    # Common constructor names are searched only with an explicit type qualifier.
    allmeth = set().union(*[set(v) for v in funcs.values()])
    allmeth.discard("new")
    method_rx = re.compile(
        r"\.\s*("
        + "|".join(sorted(map(re.escape, allmeth), key=len, reverse=True))
        + r")\s*(?:\(|::\s*<)"
    )
    qualified_rx = re.compile(
        r"\b(ConnectionRegistry|ConnectionEntry)\s*::\s*("
        + "|".join(sorted(map(re.escape, allmeth | {"new"}), key=len, reverse=True))
        + r")\s*(?:\(|::\s*<)"
    )
    field_rx = {
        k: re.compile(
            r"\.\s*("
            + "|".join(sorted(map(re.escape, v), key=len, reverse=True))
            + r")\b(?!\s*\()"
        )
        for k, v in members.items()
    }
    results = collections.defaultdict(lambda: collections.defaultdict(set))
    named = {
        "Dual registration": re.compile(
            r"\b(?:mirror_register_outcome|mirror_unregister)\s*\("
        ),
        "User and resource mirrors": re.compile(
            r"\b(?:UserRegistryActor|UserActor|RegisterUserResource(?:IfOwnerOrAbsent)?|"
            r"UnregisterUserResource|remote_hosted_for_connection|"
            r"apply_remote_resource_state|update_remote_user_resource_on_owner|"
            r"RemoteResourceStateSnapshot|spawn_remote_resource_forwarder|"
            r"sweep_remote_owner_resources)\b"
        ),
        "Room dispatch": re.compile(
            r"\b(?:dispatch_to_room|RoomMessageRoute|plan_remote_room|try_proxy_muc_remote_decision)\b"
        ),
        "Remote resource relay": re.compile(
            r"\b(?:RelayRegisterRemoteUserResource|RelayUnregisterRemoteUserResource|"
            r"RelayUpdateRemoteUserResource|RelayRemoteUserSideEffect|"
            r"RelayDeliverRemoteResourceFrame|RemoteResourceOutboundFrame|"
            r"RemoteResourceRegistrationId|deliver_remote_resource_frame|"
            r"resource_presence)\b"
        ),
    }

    paths = subprocess.check_output(
        ["rg", "--files", "server/crates", "-g", "*.rs"], text=True
    ).splitlines()
    for pathstr in paths:
        if classify(pathstr, 1) != "prod":
            continue
        path = pathlib.Path(pathstr)
        code = strip(path.read_text(errors="replace"))
        for family, rx in named.items():
            for match in rx.finditer(code):
                line = line_at(code, match.start())
                if classify(pathstr, line) == "prod":
                    results[("named", family)][pathstr].add(line)
        for match in method_rx.finditer(code):
            name = match.group(1)
            line = line_at(code, match.start(1))
            if classify(pathstr, line) != "prod":
                continue
            for k in funcs:
                if name in funcs[k]:
                    results[(k + " method", name)][pathstr].add(line)
        for match in qualified_rx.finditer(code):
            k = "Registry" if match.group(1) == "ConnectionRegistry" else "Entry"
            line = line_at(code, match.start(2))
            if classify(pathstr, line) == "prod":
                results[(k + " method", match.group(2))][pathstr].add(line)
        for k, rx in field_rx.items():
            if k == "Registry" and not pathstr.startswith(xm + "connection_registry"):
                continue
            if k == "Socket" and not pathstr.startswith(ws):
                continue
            for match in rx.finditer(code):
                name = match.group(1)
                line = line_at(code, match.start(1))
                if classify(pathstr, line) == "prod":
                    results[(k + " field", name)][pathstr].add(line)

    output_directory = pathlib.Path(args.output).absolute().parent

    def relative_link(path):
        """Keep links valid when the output is placed outside docs/planning."""
        return quote(os.path.relpath(path, output_directory), safe="/")

    def source(path, line, label=None):
        return f"[{label or str(line)}]({relative_link(path)}#L{line})"

    def locations(group):
        pairs = [
            (path, line)
            for path, lines in sorted(group.items())
            for line in sorted(lines)
        ]
        shown = pairs if args.all else pairs[:8]
        links = [
            source(path, line, f"{path.replace('server/crates/','')}:{line}")
            for path, line in shown
        ]
        if len(shown) < len(pairs):
            links.append(f"{len(pairs)-len(shown)} more (use `--all`)")
        return ", ".join(links) or "—"

    snapshot = relative_link("docs/planning/2046-current-inventory-wiring.md")
    out = [
        "# Textual candidate index for #2046 at `b2597ee`",
        "",
        "This is a deliberately overinclusive navigation index, not a type-resolved production call graph. Names can collide across types, conditionally compiled items can be included, and aliases, macros, destructured field borrows and trait dispatch can be missed. Counts count matching source lines, not semantic operations. Up to eight candidates are shown per row by default; use `--all` for all matching locations. "
        f"See [the snapshot]({snapshot}) for method, classes and limits.",
        "",
    ]
    for k in ("Registry", "Entry", "Socket"):
        out += [
            f"## {k} fields",
            "",
            "| Field | Class | Declaration | Candidate dot-access locations |",
            "| --- | --- | --- | --- |",
        ]
        for name, (path, line) in members[k].items():
            group = results[(k + " field", name)]
            out.append(
                f"| `{name}` | {classes[k][name]} | {source(path,line)} | {locations(group)} |"
            )
        out.append("")
    for k in ("Registry", "Entry"):
        out += [
            f"## {k} methods",
            "",
            "All matching receiver names are candidates only; qualified calls are included and `new` is limited to an explicit type qualifier. Consult the source before treating a hit as a use of this type.",
            "",
            "| Method | Declaration | Candidate call locations |",
            "| --- | --- | --- |",
        ]
        for name, (path, line) in sorted(funcs[k].items()):
            out.append(
                f'| `{name}` | {source(path,line)} | {locations(results[(k+" method",name)])} |'
            )
        out.append("")
    out += [
        "## Named routing and mirror candidates",
        "",
        "Each row indexes matching lines by family. These are textual candidates, including definitions and unrelated name collisions, not a closed list of semantic callers.",
        "",
        "| Family | Class | Candidate locations |",
        "| --- | --- | --- |",
    ]
    family_classes = {
        "Dual registration": "O",
        "User and resource mirrors": "A/I/O",
        "Room dispatch": "A/T",
        "Remote resource relay": "T/O",
    }
    for family in named:
        out.append(
            f'| {family} | {family_classes[family]} | {locations(results[("named",family)])} |'
        )
    pathlib.Path(args.output).write_text("\n".join(out) + "\n")
    for k in ("Registry", "Entry", "Socket"):
        print(
            k,
            "fields",
            len(members[k]),
            "field candidate lines",
            sum(
                len(v)
                for name in members[k]
                for v in results[(k + " field", name)].values()
            ),
            "methods",
            len(funcs.get(k, {})),
            "method candidate lines",
            sum(
                len(v)
                for name in funcs.get(k, {})
                for v in results[(k + " method", name)].values()
            ),
        )

    for family in named:
        print(
            family,
            "candidate lines",
            sum(len(v) for v in results[("named", family)].values()),
            "files",
            len(results[("named", family)]),
        )


if __name__ == "__main__":
    main()
