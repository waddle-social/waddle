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


# Class and read/write of every production method. A method that reads and
# writes is listed as a write. The scan fails when a method is missing here
# or listed here and gone from the source, so a new method cannot enter the
# inventory unclassified.
method_classes = {
    "Registry": {
        "new": ("I", "W"), "set_sm_stream_id": ("I", "W"),
        "set_occupancy_session_if_owner": ("I", "W"), "occupancy_session_of": ("I", "R"),
        "set_sm_stream_id_if_owner": ("I", "W"), "sm_stream_owner": ("I", "R"),
        "pending_subscription_count": ("I", "R"), "presence_state_count": ("I", "R"),
        "last_activity_count": ("I", "R"), "local_sm_stream": ("I", "R"),
        "try_lock_bind": ("T", "W"), "probe_socket_lifecycle": ("T", "R"),
        "lock_bind": ("T", "W"), "prune_completed_bind": ("T", "W"),
        "queue_pending_subscription_stanza": ("A", "W"),
        "remove_pending_subscribe": ("A", "W"),
        "drain_pending_subscription_stanzas": ("A", "W"),
        "pending_subscription_stanzas": ("A", "R"), "register": ("I", "W"),
        "register_with_carbons": ("I", "W"), "register_with_stream_state": ("I", "W"),
        "register_entry": ("I", "W"), "register_entry_if_owner_or_absent": ("I", "W"),
        "unregister": ("I", "W"), "is_owned_by": ("I", "R"),
        "unregister_if_owner": ("I", "W"), "unregister_if_sm_stream_id": ("I", "W"),
        "entry_if_owner": ("I", "R"), "is_connected": ("I", "R"),
        "is_locally_hosted": ("I", "R"), "entry": ("I", "R"),
        "connection_count": ("I", "R"), "mark_roster_interested": ("A", "W"),
        "is_roster_interested": ("A", "R"),
        "roster_interested_resources_for_user": ("A", "R"),
        "mark_blocklist_interested": ("A", "W"), "is_blocklist_interested": ("A", "R"),
        "blocklist_interested_resources_for_user": ("A", "R"),
        "send_to_many": ("T", "W"), "list_connections": ("I", "R"),
        "active_sm_stream_ids": ("I", "R"), "other_resources_for_user": ("A", "R"),
        "other_carbon_resources_for_user": ("A", "R"), "is_carbons_enabled": ("A", "R"),
        "set_carbons_enabled": ("A", "W"), "update_presence": ("A", "W"),
        "update_presence_if_owner": ("A", "W"),
        "update_presence_state_if_owner": ("A", "W"),
        "update_presence_state": ("A", "W"), "presence_state": ("A", "R"),
        "clear_presence_state": ("A", "W"), "clear_presence_state_if_owner": ("A", "W"),
        "record_last_activity": ("A", "W"), "last_activity": ("A", "R"),
        "clear_last_activity": ("A", "W"), "server_uptime_seconds": ("T", "R"),
        "available_resources_for_user": ("A", "R"), "cleanup_stale": ("I", "W"),
        "reserve_pending_flush": ("T", "W"), "try_reserve_pending_flush": ("T", "W"),
        "send_reserved_pending_flush": ("T", "W"),
        "send_to_with_write_acceptance": ("T", "W"), "send_to": ("T", "W"),
        "send_to_if_owner": ("T", "W"), "send_pending_flush": ("T", "W"),
        "send_pending_flush_if_owner": ("T", "W"), "try_send_to": ("T", "W"),
        "try_send_outbound_to_locally_hosted": ("T", "W"),
        "try_send_outbound_if_owner": ("T", "W"),
        "capture_owned_enqueue_if_owner": ("T", "W"),
        "remove_if_sender_closed": ("I", "W"), "remove_if_sender_closed_owner": ("I", "W"),
        "activate_provisional_if_confirmed": ("A/T", "W"),
        "local_binding_permit_if_owner": ("T", "R"),
        "local_sm_enable_binding_permit_if_owner": ("T", "R"),
        "mark_blocklist_interested_if_owner": ("A", "W"),
        "mark_roster_interested_if_owner": ("A", "W"),
        "publish_provisional_if_expected": ("I/T", "W"),
        "register_socket_entry": ("I", "W"), "remove_closed_owner_entry": ("I", "W"),
        "remove_retired_presence": ("A", "W"), "remove_sm_stream_owner": ("I", "W"),
        "retire_provisional_if_exact": ("I/T", "W"),
        "set_carbons_enabled_if_owner": ("A", "W"), "try_send_to_matching": ("T", "W"),
    },
    "Entry": {
        "activate_binding": ("T", "W"), "actor_custody": ("T", "R"),
        "binding_id": ("A", "R"), "binding_ref": ("A", "R"),
        "claim_offline_flush": ("A", "W"), "claim_pending_subscribes_flush": ("A", "W"),
        "commit_pending_flush": ("T", "W"), "connection_ref": ("A", "R"),
        "force_detach_sender": ("T", "R"),
        "into_socket_outbound_endpoint_if_empty": ("T", "W"),
        "into_socket_outbound_endpoint_if_receiver_empty": ("T", "W"),
        "is_carbons_enabled": ("A", "R"), "is_locally_hosted": ("I", "R"),
        "is_presence_available": ("A", "R"), "is_provisional": ("T", "R"),
        "local_io_relinquishment": ("T", "R"), "new": ("T", "W"),
        "new_actor_provisional_for_binding": ("T", "W"), "new_entry": ("T", "W"),
        "new_for_binding": ("T", "W"), "new_for_binding_with_local_io": ("T", "W"),
        "new_for_connection": ("T", "W"), "new_provisional_for_binding": ("T", "W"),
        "occupancy_session": ("I", "R"), "presence_priority": ("A", "R"),
        "remote_hosted": ("O", "W"), "remote_hosted_for_connection": ("O", "W"),
        "reset_offline_flush": ("A", "W"), "resume_control": ("T", "R"),
        "same_exact_local_binding": ("I", "R"), "seal_actor_binding_if_any": ("T", "W"),
        "seal_binding": ("T", "W"), "seal_binding_for_failed_resume": ("T", "W"),
        "set_sm_stream_id": ("I", "W"), "sm_stream_id": ("I", "R"),
        "take_force_detach_rx": ("T", "W"),
        "try_acquire_terminal_ordering_retry": ("T", "W"),
        "try_force_detach_and_interrupt": ("T", "W"),
        "try_force_detach_and_interrupt_after_offer": ("T", "W"),
        "try_force_detach_and_revoke": ("T", "W"),
        "try_force_detach_and_revoke_after_reserve": ("T", "W"),
        "try_force_detach_for_jid": ("T", "W"), "try_force_detach_raw": ("T", "W"),
        "try_send_archive_ordered": ("T", "W"),
        "try_send_archive_ordered_with_refusal": ("T", "W"),
        "wire_resume_control": ("T", "W"),
    },
}

# What follows a field access when the line changes the field. Textual: a
# `&mut` borrow taken on another line, or a write through a destructured
# binding, is not seen.
write_like = re.compile(
    r"\s*(?:=(?!=)|[-+|&]=|\.\s*(?:store|insert|remove|push|push_back|take|set|clear|swap|"
    r"replace|extend|retain|drain|pop_front|pop_back|pop|fetch_add|fetch_sub|fetch_or|"
    r"fetch_and|get_or_insert_with|get_or_insert|entry|send|try_send|lock|write)\s*\()"
)

GENERATED_START = "<!-- generated by 2046-current-caller-scan.py: start -->"
GENERATED_END = "<!-- generated by 2046-current-caller-scan.py: end -->"


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
    category = "Registry" if ty == "ConnectionRegistry" else "Entry"
    known = method_classes[category]
    unclassified = set(result) - set(known)
    missing = set(known) - set(result)
    if unclassified or missing:
        raise ValueError(
            f"{ty} methods: unclassified={sorted(unclassified)}, missing={sorted(missing)}"
        )
    return result


def short(path):
    for prefix, name in (
        ("server/crates/waddle-server/src/server/routes/websocket/", "ws/"),
        ("server/crates/waddle-server/src/server/routes/interpret/", "interpret/"),
        ("server/crates/waddle-server/src/clustering/", "clustering/"),
        ("server/crates/waddle-server/src/server/", "server/"),
        ("server/crates/waddle-server/src/", "srv/"),
        ("server/crates/waddle-xmpp/src/", "xmpp/"),
    ):
        if path.startswith(prefix):
            return name + path[len(prefix) :]
    return path


def sites(group):
    """Every site, grouped by file: `ws/frame.rs:10,12`."""
    return (
        "; ".join(
            f"`{short(path)}:{','.join(str(line) for line in sorted(lines))}`"
            for path, lines in sorted(group.items())
        )
        or "none"
    )


def total(group):
    return sum(len(lines) for lines in group.values())


def write_inventory(path, commit, members, funcs, results, writes, named):
    """Replace the generated block of the caller inventory with current tables."""
    target = pathlib.Path(path)
    text = target.read_text()
    start = text.index(GENERATED_START)
    end = text.index(GENERATED_END)
    registry_module = xm + "connection_registry"
    out = [
        GENERATED_START,
        "",
        f"Generated at `{commit}` by `docs/planning/2046-current-caller-scan.py --inventory`.",
        "Sites are source lines that match textually outside test code, written",
        "`file:line,line`. \"Write-like\" counts the sites where the field is assigned,",
        "borrowed mutably on the same expression, or has a mutating method called on it.",
        "",
    ]
    titles = {
        "Registry": ("1", "ConnectionRegistry"),
        "Entry": ("2", "ConnectionEntry"),
        "Socket": ("3", "WsConnState"),
    }
    for category in ("Registry", "Entry", "Socket"):
        number, ty = titles[category]
        declared = structfiles[category][0]
        out += [
            f"## {number}. `{ty}` (`{short(str(declared))}`)",
            "",
            f"### {number}.1 Fields",
            "",
            "| Field | Class | Declared | Sites | Write-like | Files | Every site |",
            "| --- | --- | --- | --- | --- | --- | --- |",
        ]
        for name, (decl_path, line) in members[category].items():
            group = results[(category + " field", name)]
            out.append(
                f"| `{name}` | {classes[category][name]} | `{short(decl_path)}:{line}` | "
                f"{total(group)} | {total(writes[(category + ' field', name)])} | "
                f"{len(group)} | {sites(group)} |"
            )
        out.append("")
        if category not in funcs:
            continue
        out += [
            f"### {number}.2 Methods",
            "",
            "\"Outside\" counts call sites outside `xmpp/registry/connection_registry*`;",
            "\"inside\" counts those within it. A receiver of another type with the same",
            "method name is counted.",
            "",
            "| Method | Class | R/W | Declared | Outside | Inside | Every site outside |",
            "| --- | --- | --- | --- | --- | --- | --- |",
        ]
        for name, (decl_path, line) in sorted(funcs[category].items()):
            group = results[(category + " method", name)]
            outside = {
                p: lines for p, lines in group.items() if not p.startswith(registry_module)
            }
            inside = total(group) - total(outside)
            klass, access = method_classes[category][name]
            out.append(
                f"| `{name}` | {klass} | {access} | `{short(decl_path)}:{line}` | "
                f"{total(outside)} | {inside} | {sites(outside)} |"
            )
        out.append("")
    family_classes = {
        "Dual registration": ("4", "O"),
        "User and resource mirrors": ("5", "A/I/O"),
        "Remote resource relay": ("6", "T/O"),
        "Room dispatch": ("7", "A/T"),
    }
    out += [
        "## Current sites of sections 4 to 7",
        "",
        "Every production line that names one of the identifiers each section is",
        "about, definitions included. The analysis in sections 4 to 7 below was",
        "written against an earlier commit; its line numbers are not current, these",
        "are.",
        "",
        "| Section | Family | Class | Sites | Files | Every site |",
        "| --- | --- | --- | --- | --- | --- |",
    ]
    for family, (number, klass) in family_classes.items():
        group = results[("named", family)]
        out.append(
            f"| {number} | {family} | {klass} | {total(group)} | {len(group)} | {sites(group)} |"
        )
    out.append("")
    target.write_text(text[:start] + "\n".join(out) + text[end:])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--all", action="store_true", help="include every textual candidate link"
    )
    parser.add_argument(
        "--output", default="docs/planning/2046-current-caller-candidates.md"
    )
    parser.add_argument(
        "--inventory",
        help="rewrite the generated sections of this caller inventory in place",
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
    writes = collections.defaultdict(lambda: collections.defaultdict(set))
    named = {
        "Dual registration": re.compile(
            r"\b(?:mirror_register|mirror_register_outcome|mirror_unregister)\s*\("
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
                    borrowed = code[max(0, match.start() - 160) : match.start()]
                    if write_like.match(code, match.end()) or re.search(
                        r"&mut\s+[\w.]*$", borrowed
                    ):
                        writes[(k + " field", name)][pathstr].add(line)

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

    commit = subprocess.check_output(
        ["git", "rev-parse", "--short=9", "HEAD"], text=True
    ).strip()
    if args.inventory:
        write_inventory(args.inventory, commit, members, funcs, results, writes, named)
    snapshot = relative_link("docs/planning/2046-current-inventory-wiring.md")
    out = [
        f"# Textual candidate index for #2046 at `{commit}`",
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
