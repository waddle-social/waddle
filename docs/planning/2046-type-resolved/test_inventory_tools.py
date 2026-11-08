#!/usr/bin/env python3
"""Focused negative and positive checks for #2046 inventory tooling."""

from __future__ import annotations

import copy
import contextlib
import csv
import gzip
import hashlib
import io
import json
import pathlib
import subprocess
import tempfile
import unittest
from unittest import mock

import mark_targets as marker
import parse_diagnostics as parser
import parse_test_diagnostics as test_capture
import validate_inventory as inventory


class DiscoveryTests(unittest.TestCase):
    def test_registration_messages_and_handlers_are_discovered(self) -> None:
        text = '''
// impl Message<Fake> for UserActor { async fn handle() {} }
pub struct RegisterConnection { pub jid: Jid }
pub struct UnregisterConnection { pub jid: Jid }
impl kameo::message::Message<RegisterConnection> for UserActor {
    async fn handle(&mut self, msg: RegisterConnection) {}
}
impl kameo::message::Message<UnregisterConnection> for UserActor {
    async fn handle(&mut self, msg: UnregisterConnection) {}
}
'''
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "actor.rs").write_text(text)
            handlers = marker.discover_message_handlers(root, "actor.rs", "UserActor")
            self.assertEqual({h["message"] for h in handlers}, {"RegisterConnection", "UnregisterConnection"})
            self.assertTrue(all(h["review_status"] == "manual-review-required" for h in handlers))
            self.assertTrue(all(h["cfg_test_classification"] == "manual-review-required" for h in handlers))
            targets = []
            pattern = r"^(?:.*Resource.*|" + "|".join(h["message"] for h in handlers) + r")$"
            marker.add_named_type_family(root, targets, "actor.rs", pattern, "UAMSG")
            tags = {target["tag"].split("@", 1)[0] for target in targets}
            self.assertIn("UAMSG::RegisterConnection::TYPE", tags)
            self.assertIn("UAMSG::UnregisterConnection::TYPE", tags)

    def test_unsupported_impl_and_truncated_item_fail(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "actor.rs").write_text("impl<T> Message<T> for UserActor { async fn handle() {} }\n")
            with self.assertRaisesRegex(ValueError, "unsupported"):
                marker.discover_message_handlers(root, "actor.rs", "UserActor")
            with self.assertRaisesRegex(ValueError, "unterminated"):
                marker.item_end("struct Missing {", 0)

    def test_handler_discovery_scans_all_files_including_tests_and_qualified_actors(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            expected = [("server/crates/one/src/lib.rs", "UserActor"), ("server/crates/two/src/tests.rs", "RelayActor"), ("server/crates/three/src/nested/tests.rs", "UserRegistryActor")]
            for number, (rel, actor) in enumerate(expected):
                path = root / rel
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(f"impl Message<Message{number}> for crate::module::{actor} {{\n    async fn handle() {{}}\n}}\nimpl Message<Other> for OtherActor {{\n    async fn handle() {{}}\n}}\n")
            result = marker.discover_scoped_handlers(root)
            self.assertEqual({(row["path"], row["actor"]) for row in result}, set(expected))
            self.assertEqual(len(result), len(expected))
            self.assertTrue(all(row["cfg_test_classification"] == "manual-review-required" for row in result))

    def test_all_file_scan_rejects_unsupported_impl_headers(self) -> None:
        for header in (
            "impl<T> Message<T> for UserActor",
            "impl Message<Option<Foo>> for UserActor",
            "impl Message<Value<{ 1 }>> for UserActor",
            "impl Message<Foo> for crate::UserActor where Foo: Trait",
        ):
            with self.subTest(header=header), tempfile.TemporaryDirectory() as directory:
                root = pathlib.Path(directory)
                path = root / "server/crates/example/src/tests.rs"
                path.parent.mkdir(parents=True)
                path.write_text(header + " {\n    async fn handle() {}\n}\n")
                with self.assertRaisesRegex(ValueError, "unsupported Message implementation"):
                    marker.discover_scoped_handlers(root)


class ParserTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = pathlib.Path(self.tmp.name)
        self.rel = "server/crates/example/src/lib.rs"
        self.path = self.root / self.rel
        self.path.parent.mkdir(parents=True)
        self.original = "pub struct Foo;\n"
        self.tag = "T::TYPE@L1C5"
        self.row = {"file": self.rel, "line": "1", "column": "5", "tag": self.tag, "kind": "type", "sha256_before": hashlib.sha256(self.original.encode()).hexdigest()}
        self.path.write_text(f'#[deprecated(note = "W2046|{self.tag}")]\n' + self.original)

    def test_exact_marker_manifest_and_hash(self) -> None:
        parser.validate_patched_sources(self.root, [self.row])
        self.path.write_text(self.original)
        with self.assertRaisesRegex(ValueError, "marker lines"):
            parser.validate_patched_sources(self.root, [self.row])
        self.path.write_text(f'#[deprecated(note = "W2046|{self.tag};W2046|{self.tag}")]\n' + self.original)
        with self.assertRaisesRegex(ValueError, "duplicate"):
            parser.validate_patched_sources(self.root, [self.row])
        self.path.write_text(f'#[deprecated(note = "W2046|{self.tag}")]\n' + self.original + "// stale\n")
        with self.assertRaisesRegex(ValueError, "hash mismatch"):
            parser.validate_patched_sources(self.root, [self.row])

    def test_manifest_fingerprint_rejects_mutually_truncated_evidence(self) -> None:
        manifest = self.root / "manifest.tsv"
        with manifest.open("w", newline="") as out:
            writer = csv.DictWriter(out, fieldnames=list(self.row), delimiter="\t", lineterminator="\n")
            writer.writeheader()
            writer.writerow(self.row)
        digest = marker.manifest_fingerprint([self.row])
        self.assertEqual(parser.load_manifest(manifest, expected_digest=digest), [self.row])
        with self.assertRaisesRegex(ValueError, "fingerprint"):
            parser.load_manifest(manifest)
        with manifest.open("a") as out:
            out.write("\t".join(self.row.values()) + "\n")
        with self.assertRaisesRegex(ValueError, "duplicate"):
            parser.load_manifest(manifest, expected_digest=digest)

    def test_historical_manifest_cannot_be_mistaken_for_current_source(self) -> None:
        historical = pathlib.Path(__file__).parent / "capture-5763923/targets.tsv"
        with self.assertRaisesRegex(ValueError, "pinned discovery fingerprint"):
            parser.load_manifest(historical)

    def test_cargo_stream_rejects_truncation_duplicate_and_failure(self) -> None:
        good = '{"reason":"build-finished","success":true}'
        self.assertEqual(len(parser.parse_cargo_stream([good])), 1)
        for lines in ([], ['{"reason":'], [good, good], [good, '{"reason":"compiler-message"}'], ['{"reason":"build-finished","success":false}']):
            with self.subTest(lines=lines), self.assertRaises(ValueError):
                parser.parse_cargo_stream(lines)
        self.assertEqual(parser.strip_marker_lines('#[deprecated(note = "W2046|x")]\nfn a() {}'), 'fn a() {}')

    def test_cargo_artifact_profile_and_freshness(self) -> None:
        def artifact(name: str, package: str, features: list[str]) -> dict[str, object]:
            return {"reason": "compiler-artifact", "target": {"name": name, "kind": ["lib"], "src_path": str(self.root / f"server/crates/{package}/src/lib.rs")}, "profile": dict(parser.DEV_PROFILE), "features": features, "fresh": False, "package_id": package}
        messages = [artifact("waddle_xmpp", "waddle-xmpp", []), artifact("waddle_server", "waddle-server", ["clustering"])]
        self.assertEqual(len(parser.cargo_artifacts(messages, self.root, "clustering")), 2)
        with self.assertRaisesRegex(ValueError, "features"):
            parser.cargo_artifacts(messages, self.root, "default")
        stale = copy.deepcopy(messages)
        stale[0]["fresh"] = True
        with self.assertRaisesRegex(ValueError, "rebuild"):
            parser.cargo_artifacts(stale, self.root, "clustering")
        with self.assertRaisesRegex(ValueError, "missing required"):
            parser.cargo_artifacts(messages[:1], self.root, "clustering")
        with self.assertRaisesRegex(ValueError, "duplicate"):
            parser.cargo_artifacts(messages + messages[:1], self.root, "clustering")
        for field, value in (("opt_level", "3"), ("debuginfo", "2"), ("debug_assertions", False), ("overflow_checks", False), ("test", True), ("debug_assertions", 1)):
            changed = copy.deepcopy(messages)
            changed[0]["profile"][field] = value
            with self.subTest(field=field, value=value), self.assertRaisesRegex(ValueError, "exact captured dev profile"):
                parser.cargo_artifacts(changed, self.root, "clustering")
        missing = copy.deepcopy(messages)
        del missing[0]["profile"]["overflow_checks"]
        with self.assertRaisesRegex(ValueError, "dev profile"):
            parser.cargo_artifacts(missing, self.root, "clustering")
        for index in (0, 1):
            extra = copy.deepcopy(messages)
            extra[index]["features"].append("test-utils")
            with self.subTest(index=index), self.assertRaisesRegex(ValueError, "features"):
                parser.cargo_artifacts(extra, self.root, "clustering")

    def test_recorded_paths_are_bounded_by_capture_root(self) -> None:
        root = parser.parse_capture_root("/archive/original/waddle")
        relative = "server/crates/waddle-xmpp/src/lib.rs"
        self.assertEqual(parser.normalize_file(f"{root}/{relative}", root), relative)
        self.assertEqual(parser.normalize_file("crates/waddle-xmpp/src/lib.rs", root), relative)
        self.assertEqual(parser.normalize_file(relative, root), relative)
        for invalid in (
            f"{root}-other/{relative}", "/archive/elsewhere/server/crates/waddle-xmpp/src/lib.rs",
            f"{root}/../waddle/{relative}", "crates/../crates/waddle-xmpp/src/lib.rs",
            "./crates/waddle-xmpp/src/lib.rs", "server/crates/../../outside.rs", "crates\\waddle-xmpp\\src\\lib.rs",
        ):
            with self.subTest(invalid=invalid):
                self.assertIsNone(parser.normalize_file(invalid, root))
        for invalid in ("relative/root", "/", "/archive/../waddle", "/archive//waddle", "/archive/./waddle", "/archive/waddle/"):
            with self.subTest(invalid_root=invalid), self.assertRaises(ValueError):
                parser.parse_capture_root(invalid)

    def test_artifact_capture_root_need_not_be_the_current_checkout(self) -> None:
        captured = pathlib.PurePosixPath("/an/old/checkout")
        messages = []
        for name, package in (("waddle_xmpp", "waddle-xmpp"), ("waddle_server", "waddle-server")):
            messages.append({"reason": "compiler-artifact", "target": {"name": name, "kind": ["lib"], "src_path": str(captured / f"server/crates/{package}/src/lib.rs")}, "profile": dict(parser.DEV_PROFILE), "features": [], "fresh": False, "package_id": package})
        self.assertEqual(len(parser.cargo_artifacts(messages, captured, "default")), 2)
        with self.assertRaisesRegex(ValueError, "capture root"):
            parser.cargo_artifacts(messages, pathlib.PurePosixPath("/an/old/checkout-other"), "default")
        escaped = copy.deepcopy(messages)
        escaped[0]["target"]["src_path"] = "/an/old/checkout/../checkout/server/crates/waddle-xmpp/src/lib.rs"
        with self.assertRaisesRegex(ValueError, "capture root"):
            parser.cargo_artifacts(escaped, captured, "default")

    def test_capture_hash_covers_uncompressed_jsonl_and_gzip(self) -> None:
        raw = b'{"reason":"build-finished","success":true}\n'
        digest = hashlib.sha256(raw).hexdigest()
        plain = self.root / "capture.jsonl"
        zipped = self.root / "capture.jsonl.gz"
        plain.write_bytes(raw)
        zipped.write_bytes(gzip.compress(raw))
        self.assertEqual(parser.read_capture(plain, digest), raw)
        self.assertEqual(parser.read_capture(zipped, digest), raw)
        with self.assertRaisesRegex(ValueError, "SHA-256"):
            parser.read_capture(zipped, "0" * 64)
        zipped.write_bytes(gzip.compress(raw)[:-5])
        with self.assertRaisesRegex(ValueError, "truncated gzip"):
            parser.read_capture(zipped, digest)
        plain.write_bytes(raw[:-1])
        with self.assertRaisesRegex(ValueError, "final newline"):
            parser.read_capture(plain)

    def test_reparse_detects_coordinated_uses_and_coverage_truncation(self) -> None:
        capture = self.root / "capture.jsonl.gz"
        raw = b'{"reason":"build-finished","success":true}\n'
        capture.write_bytes(gzip.compress(raw))
        digest = hashlib.sha256(raw).hexdigest()
        uses = self.root / "uses.tsv"
        coverage = self.root / "coverage.tsv"
        uses.write_bytes(b"site\nfirst\nsecond\n")
        coverage.write_bytes(b"count\n2\n")

        def fake_reparse(command: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
            self.assertEqual(command[command.index("--capture-root") + 1], "/recorded/root")
            pathlib.Path(command[command.index("--output") + 1]).write_bytes(b"site\nfirst\nsecond\n")
            pathlib.Path(command[command.index("--coverage") + 1]).write_bytes(b"count\n2\n")
            return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

        with mock.patch("validate_inventory.subprocess.run", side_effect=fake_reparse):
            inventory.validate_capture_outputs(self.root, "/recorded/root", self.root / "manifest.tsv", capture, digest, "default", uses, coverage)
            uses.write_bytes(b"site\nfirst\n")
            coverage.write_bytes(b"count\n1\n")
            with self.assertRaisesRegex(ValueError, "does not match retained Cargo capture"):
                inventory.validate_capture_outputs(self.root, "/recorded/root", self.root / "manifest.tsv", capture, digest, "default", uses, coverage)


class TestCaptureTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = pathlib.Path(self.tmp.name)
        self.captured = pathlib.PurePosixPath("/archive/original/waddle")
        (self.root / "server").mkdir()
        (self.root / "server/Cargo.toml").write_text('[workspace.package]\nversion = "0.1.0"\n')
        for package in ("waddle-server", "waddle-xmpp"):
            crate = self.root / "server/crates" / package
            (crate / "src").mkdir(parents=True)
            (crate / "src/lib.rs").write_text("pub struct Marked;\n")
            (crate / "src/main.rs").write_text("fn main() {}\n")
            (crate / "src/custom_explicit_test.rs").write_text("fn main() {}\n")
            (crate / "src/bin").mkdir()
            (crate / "src/bin/cluster.rs").write_text("fn main() {}\n")
            (crate / "tests").mkdir()
            (crate / "tests/selected.rs").write_text("fn fixture() { marker_reference(); }\n")
            (crate / "tests/support_only.rs").write_text('#![cfg(feature = "test-support")]\nfn fixture() { marker_reference(); }\n')
            (crate / "tests/support_gated.rs").write_text("fn fixture() {}\n")
            (crate / "tests/nested").mkdir()
            (crate / "tests/nested/main.rs").write_text("fn main() {}\n")
            manifest = '[package]\nname = "' + package + '"\n'
            if package == "waddle-server":
                manifest += '\n[[bin]]\nname = "waddle-server"\npath = "src/main.rs"\n'
                manifest += '\n[[bin]]\nname = "clustering-bin"\npath = "src/bin/cluster.rs"\nrequired-features = ["clustering"]\n'
            (crate / "Cargo.toml").write_text(manifest)
        packages = []
        for package in ("waddle-server", "waddle-xmpp"):
            base = self.captured / "server/crates" / package
            package_id = f"path+file://{base}#0.1.0"
            lib = "waddle_server" if package == "waddle-server" else "waddle_xmpp"
            targets = [
                {"kind": ["lib"], "name": lib, "src_path": str(base / "src/lib.rs"), "test": True},
                {"kind": ["test"], "name": "selected", "src_path": str(base / "tests/selected.rs"), "test": True},
                {"kind": ["test"], "name": "nested", "src_path": str(base / "tests/nested/main.rs"), "test": True},
                {"kind": ["test"], "name": "support_only", "src_path": str(base / "tests/support_only.rs"), "test": True},
                {"kind": ["test"], "name": "support_gated", "src_path": str(base / "tests/support_gated.rs"), "test": True, "required-features": ["test-support"]},
                {"kind": ["test"], "name": "custom", "src_path": str(base / "src/custom_explicit_test.rs"), "test": True, "required-features": ["extra"]},
            ]
            if package == "waddle-server":
                targets.extend([
                    {"kind": ["bin"], "name": "waddle-server", "src_path": str(base / "src/main.rs"), "test": True},
                    {"kind": ["bin"], "name": "clustering-bin", "src_path": str(base / "src/bin/cluster.rs"), "test": True, "required-features": ["clustering"]},
                ])
            packages.append({"id": package_id, "name": package, "version": "0.1.0", "source": None, "manifest_path": str(base / "Cargo.toml"), "targets": targets})
        metadata = {"version": 1, "workspace_root": str(self.captured / "server"), "workspace_members": [item["id"] for item in packages], "packages": packages}
        self.metadata_path = self.root / "metadata.json"
        self.metadata_path.write_text(json.dumps(metadata) + "\n")
        self.metadata_digest = hashlib.sha256(self.metadata_path.read_bytes()).hexdigest()
        self.metadata = test_capture.load_metadata(self.metadata_path, self.metadata_digest, self.root, self.captured)
        classifier = self.root / "docs/planning/connection_caller_regions.py"
        classifier.parent.mkdir(parents=True)
        classifier.write_text('def classify(path, line):\n    return "prod"\n')

    def artifact(self, package: str, kind: str, name: str, test: bool, features: list[str], *, source: str | None = None) -> dict[str, object]:
        if source is None:
            source = "src/lib.rs" if kind == "lib" else f"tests/{name}.rs"
        return {
            "reason": "compiler-artifact", "package_id": f"path+file://{self.captured}/server/crates/{package}#0.1.0",
            "target": {"kind": [kind], "name": name, "src_path": str(self.captured / "server/crates" / package / source)},
            "profile": {**parser.DEV_PROFILE, "test": test}, "features": list(features), "fresh": False,
        }

    def test_mode_inventories_are_explicit_and_profile_specific(self) -> None:
        server = lambda test, features: self.artifact("waddle-server", "lib", "waddle_server", test, features)
        xmpp = lambda test: self.artifact("waddle-xmpp", "lib", "waddle_xmpp", test, ["test-utils"])
        cases = [
            ("support-lib", "default", set(), [server(False, ["test-support"]), xmpp(False)]),
            ("support-lib", "clustering", set(), [server(False, ["clustering", "test-support"]), xmpp(False)]),
            ("server-unit", "default", set(), [server(True, []), xmpp(False)]),
            ("server-unit", "clustering", set(), [server(True, ["clustering"]), xmpp(False)]),
            ("server-integration", "default", {"selected"}, [server(False, []), xmpp(False), self.artifact("waddle-server", "test", "selected", True, [])]),
            ("server-integration", "clustering", {"selected"}, [server(False, ["clustering"]), xmpp(False), self.artifact("waddle-server", "test", "selected", True, ["clustering"])]),
            ("server-support-integration", "default", {"selected"}, [server(False, ["test-support"]), xmpp(False), self.artifact("waddle-server", "test", "selected", True, ["test-support"])]),
            ("server-support-integration", "clustering", {"selected"}, [server(False, ["clustering", "test-support"]), xmpp(False), self.artifact("waddle-server", "test", "selected", True, ["clustering", "test-support"])]),
            ("xmpp-unit", "test-utils", set(), [xmpp(True)]),
            ("xmpp-integration", "test-utils", {"selected"}, [xmpp(False), self.artifact("waddle-xmpp", "test", "selected", True, ["test-utils"])]),
        ]
        for mode, profile, selected, messages in cases:
            with self.subTest(mode=mode, profile=profile):
                artifacts = test_capture.test_inventory(messages, self.root, self.captured, mode, profile, selected, self.metadata)
                self.assertEqual(len(artifacts), len(messages))
        targets = self.metadata["waddle-server"]["targets"]
        self.assertEqual(targets[("test", "custom")]["source"], "server/crates/waddle-server/src/custom_explicit_test.rs")
        self.assertEqual(targets[("test", "custom")]["required_features"], ["extra"])

    def test_support_integration_rejects_missing_or_extra_features_on_every_artifact(self) -> None:
        for profile, features in (("default", ["test-support"]), ("clustering", ["clustering", "test-support"])):
            messages = [
                self.artifact("waddle-server", "lib", "waddle_server", False, features),
                self.artifact("waddle-xmpp", "lib", "waddle_xmpp", False, ["test-utils"]),
                self.artifact("waddle-server", "test", "selected", True, features),
            ]
            for index, artifact in enumerate(messages):
                for missing in artifact["features"]:
                    bad = copy.deepcopy(messages)
                    bad[index]["features"].remove(missing)
                    with self.subTest(profile=profile, index=index, missing=missing), self.assertRaisesRegex(ValueError, "feature set"):
                        test_capture.test_inventory(bad, self.root, self.captured, "server-support-integration", profile, {"selected"}, self.metadata)
                bad = copy.deepcopy(messages)
                bad[index]["features"].append("unexpected")
                with self.subTest(profile=profile, index=index, extra=True), self.assertRaisesRegex(ValueError, "feature set"):
                    test_capture.test_inventory(bad, self.root, self.captured, "server-support-integration", profile, {"selected"}, self.metadata)
            with self.subTest(profile=profile, mode="server-integration"), self.assertRaisesRegex(ValueError, "feature set"):
                test_capture.test_inventory(messages, self.root, self.captured, "server-integration", profile, {"selected"}, self.metadata)

    def test_support_integration_selection_and_bins_use_metadata(self) -> None:
        for profile, features in (("default", ["test-support"]), ("clustering", ["clustering", "test-support"])):
            server = self.artifact("waddle-server", "lib", "waddle_server", False, features)
            xmpp = self.artifact("waddle-xmpp", "lib", "waddle_xmpp", False, ["test-utils"])
            selected = self.artifact("waddle-server", "test", "support_gated", True, features)
            binary = self.artifact("waddle-server", "bin", "waddle-server", False, features, source="src/main.rs")
            messages = [server, xmpp, selected, binary]
            artifacts = test_capture.test_inventory(messages, self.root, self.captured, "server-support-integration", profile, {"support_gated"}, self.metadata)
            self.assertFalse(artifacts[(binary["package_id"], "bin", "waddle-server")]["test"])
            with self.subTest(profile=profile, selected="missing"), self.assertRaisesRegex(ValueError, "unknown or feature-ineligible selected"):
                test_capture.test_inventory(messages, self.root, self.captured, "server-support-integration", profile, {"missing"}, self.metadata)
            with self.subTest(profile=profile, selected="not compiled"), self.assertRaisesRegex(ValueError, "missing required"):
                test_capture.test_inventory([server, xmpp, binary], self.root, self.captured, "server-support-integration", profile, {"support_gated"}, self.metadata)
            with self.subTest(profile=profile, selected="unselected artifact"), self.assertRaisesRegex(ValueError, "unknown or unselected"):
                test_capture.test_inventory(messages, self.root, self.captured, "server-support-integration", profile, {"selected"}, self.metadata)
            bad_binary = copy.deepcopy(binary)
            bad_binary["profile"]["test"] = True
            with self.subTest(profile=profile, binary="wrong test mode"), self.assertRaisesRegex(ValueError, "profile"):
                test_capture.test_inventory([server, xmpp, selected, bad_binary], self.root, self.captured, "server-support-integration", profile, {"support_gated"}, self.metadata)
            bad_binary = copy.deepcopy(binary)
            bad_binary["features"] = []
            with self.subTest(profile=profile, binary="wrong features"), self.assertRaisesRegex(ValueError, "feature set"):
                test_capture.test_inventory([server, xmpp, selected, bad_binary], self.root, self.captured, "server-support-integration", profile, {"support_gated"}, self.metadata)
            cluster_bin = self.artifact("waddle-server", "bin", "clustering-bin", False, features, source="src/bin/cluster.rs")
            if profile == "clustering":
                artifacts = test_capture.test_inventory([*messages, cluster_bin], self.root, self.captured, "server-support-integration", profile, {"support_gated"}, self.metadata)
                self.assertIn((cluster_bin["package_id"], "bin", "clustering-bin"), artifacts)
            else:
                with self.assertRaisesRegex(ValueError, "feature-ineligible metadata target"):
                    test_capture.test_inventory([*messages, cluster_bin], self.root, self.captured, "server-support-integration", profile, {"support_gated"}, self.metadata)
            with self.subTest(profile=profile, old_mode=True), self.assertRaisesRegex(ValueError, "feature-ineligible selected"):
                test_capture.test_inventory(messages, self.root, self.captured, "server-integration", profile, {"support_gated"}, self.metadata)

    def test_support_integration_all_targets_selects_support_required_metadata_targets(self) -> None:
        tag = "EXAMPLE::TYPE@L1C1"
        manifest = [{"file": "server/crates/waddle-server/src/lib.rs", "line": "1", "tag": tag}]
        for profile, features in (("default", ["test-support"]), ("clustering", ["clustering", "test-support"])):
            messages = [
                self.artifact("waddle-server", "lib", "waddle_server", False, features),
                self.artifact("waddle-xmpp", "lib", "waddle_xmpp", False, ["test-utils"]),
                self.artifact("waddle-server", "test", "selected", True, features),
                self.artifact("waddle-server", "test", "nested", True, features, source="tests/nested/main.rs"),
                self.artifact("waddle-server", "test", "support_only", True, features),
                self.artifact("waddle-server", "test", "support_gated", True, features),
            ]
            capture_path = self.root / f"{profile}.jsonl"
            output = self.root / f"{profile}-uses.tsv"
            coverage = self.root / f"{profile}-coverage.tsv"
            inventory_path = self.root / f"{profile}-inventory.tsv"

            def run_capture(records: list[dict[str, object]]) -> dict[str, object]:
                capture_path.write_text("".join(json.dumps(record) + "\n" for record in [*records, {"reason": "build-finished", "success": True}]))
                args = ["parse_test_diagnostics.py", "--capture-mode", "server-support-integration", "--feature-profile", profile,
                        "--all-test-targets", "--metadata", str(self.metadata_path), "--metadata-sha256", self.metadata_digest,
                        "--diagnostics", str(capture_path), "--capture-sha256", hashlib.sha256(capture_path.read_bytes()).hexdigest(),
                        "--capture-root", str(self.captured), "--patched-root", str(self.root), "--manifest", str(self.root / "manifest.tsv"),
                        "--output", str(output), "--coverage", str(coverage), "--inventory", str(inventory_path)]
                stdout = io.StringIO()
                with mock.patch("sys.argv", args), mock.patch.object(test_capture, "load_manifest", return_value=manifest), \
                        mock.patch.object(test_capture, "validate_patched_checkout"), contextlib.redirect_stdout(stdout):
                    test_capture.main()
                return json.loads(stdout.getvalue())

            with self.subTest(profile=profile):
                summary = run_capture(messages)
                self.assertEqual(summary["selected_integration_targets"], ["nested", "selected", "support_gated", "support_only"])
                self.assertEqual(summary["typed_use_sites"], 0)
                self.assertIn("cfg-gated target bodies", summary["evidence_limit"])
                with inventory_path.open(newline="") as source:
                    rows = list(csv.DictReader(source, delimiter="\t"))
                self.assertEqual(len(rows), len(messages))
                self.assertTrue(all(row["capture_mode"] == "server-support-integration" for row in rows))
                with self.assertRaisesRegex(SystemExit, "missing required Cargo targets"):
                    run_capture(messages[:-1])

    def test_cfg_gated_target_artifact_alone_does_not_imply_body_compilation(self) -> None:
        tag = "EXAMPLE::TYPE@L1C1"
        manifest = [{"file": "server/crates/waddle-server/src/lib.rs", "line": "1", "tag": tag}]
        xmpp = self.artifact("waddle-xmpp", "lib", "waddle_xmpp", False, ["test-utils"])
        for mode, features in (("server-integration", []), ("server-support-integration", ["test-support"])):
            server = self.artifact("waddle-server", "lib", "waddle_server", False, features)
            selected = self.artifact("waddle-server", "test", "support_only", True, features)
            messages = [server, xmpp, selected]
            artifacts = test_capture.test_inventory(messages, self.root, self.captured, mode, "default", {"support_only"}, self.metadata)
            rows = test_capture.diagnostic_rows(messages, self.root, self.captured, manifest, artifacts, mode, "default", "a" * 64, "b" * 64)
            self.assertEqual(rows, [])
            self.assertTrue(artifacts[(selected["package_id"], "test", "support_only")]["test"])
        diagnostic = {
            "reason": "compiler-message", "package_id": selected["package_id"], "target": copy.deepcopy(selected["target"]),
            "message": {"code": {"code": "deprecated"}, "message": "use of deprecated item W2046|" + tag,
                        "spans": [{"is_primary": True, "file_name": "crates/waddle-server/tests/support_only.rs", "line_start": 2, "column_start": 4}]},
        }
        rows = test_capture.diagnostic_rows([diagnostic], self.root, self.captured, manifest, artifacts, "server-support-integration", "default", "a" * 64, "b" * 64)
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["compiled_target_name"], "support_only")
        self.assertEqual(json.loads(rows[0]["compiled_target_features"]), ["test-support"])
        self.assertIn("marker_reference", rows[0]["source"])

    def test_dual_artifacts_are_ambiguous_regardless_of_order(self) -> None:
        unit = self.artifact("waddle-server", "lib", "waddle_server", True, [])
        regular = self.artifact("waddle-server", "lib", "waddle_server", False, [])
        xmpp = self.artifact("waddle-xmpp", "lib", "waddle_xmpp", False, ["test-utils"])
        for pair in ([unit, regular], [regular, unit]):
            with self.subTest(pair=pair), self.assertRaisesRegex(ValueError, "ambiguous"):
                test_capture.test_inventory([*pair, xmpp], self.root, self.captured, "server-unit", "default", set(), self.metadata)

    def test_missing_unknown_and_wrong_artifacts_fail_closed(self) -> None:
        server = self.artifact("waddle-server", "lib", "waddle_server", False, [])
        xmpp = self.artifact("waddle-xmpp", "lib", "waddle_xmpp", False, ["test-utils"])
        selected = self.artifact("waddle-server", "test", "selected", True, [])
        with self.assertRaisesRegex(ValueError, "missing required"):
            test_capture.test_inventory([server, xmpp], self.root, self.captured, "server-integration", "default", {"selected"}, self.metadata)
        with self.assertRaisesRegex(ValueError, "unknown or feature-ineligible selected"):
            test_capture.test_inventory([server, xmpp], self.root, self.captured, "server-integration", "default", {"missing"}, self.metadata)
        with self.assertRaisesRegex(ValueError, "unknown or feature-ineligible selected"):
            test_capture.test_inventory([server, xmpp], self.root, self.captured, "server-integration", "default", {"custom"}, self.metadata)
        unknown = self.artifact("waddle-server", "test", "nested", True, [], source="tests/nested/main.rs")
        with self.assertRaisesRegex(ValueError, "unknown or unselected"):
            test_capture.test_inventory([server, xmpp, selected, unknown], self.root, self.captured, "server-integration", "default", {"selected"}, self.metadata)
        for field, value, error in (("fresh", True, "fresh"), ("features", ["clustering"], "feature set"), ("profile", {**parser.DEV_PROFILE, "test": False}, "profile")):
            bad = copy.deepcopy(selected)
            bad[field] = value
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, error):
                test_capture.test_inventory([server, xmpp, bad], self.root, self.captured, "server-integration", "default", {"selected"}, self.metadata)
        bad_source = copy.deepcopy(selected)
        bad_source["target"]["src_path"] = str(self.captured) + "/../waddle/server/crates/waddle-server/tests/selected.rs"
        with self.assertRaisesRegex(ValueError, "target source"):
            test_capture.test_inventory([server, xmpp, bad_source], self.root, self.captured, "server-integration", "default", {"selected"}, self.metadata)
        with self.assertRaisesRegex(ValueError, "build-finished"):
            parser.parse_cargo_stream(['{"reason":"compiler-artifact"}'])

    def test_diagnostic_uses_artifact_test_mode_not_source_hint(self) -> None:
        server = self.artifact("waddle-server", "lib", "waddle_server", False, [])
        xmpp = self.artifact("waddle-xmpp", "lib", "waddle_xmpp", False, ["test-utils"])
        selected = self.artifact("waddle-server", "test", "selected", True, [])
        tag = "EXAMPLE::TYPE@L1C1"
        diagnostic = {
            "reason": "compiler-message", "package_id": selected["package_id"],
            "target": copy.deepcopy(selected["target"]),
            "message": {"code": {"code": "deprecated"}, "message": "use of deprecated item W2046|" + tag,
                        "spans": [{"is_primary": True, "file_name": "crates/waddle-server/tests/selected.rs", "line_start": 1, "column_start": 4}]},
        }
        messages = [server, xmpp, selected, diagnostic]
        artifacts = test_capture.test_inventory(messages, self.root, self.captured, "server-integration", "default", {"selected"}, self.metadata)
        manifest = [{"file": "server/crates/waddle-server/src/lib.rs", "line": "1", "tag": tag}]
        rows = test_capture.diagnostic_rows(messages, self.root, self.captured, manifest, artifacts, "server-integration", "default", "a" * 64, "b" * 64)
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["source_class_hint"], "prod")
        self.assertEqual(rows[0]["compiled_target_test_mode"], "true")
        self.assertEqual(rows[0]["compiled_package_id"], selected["package_id"])
        unknown = copy.deepcopy(diagnostic)
        unknown["target"]["name"] = "another"
        with self.assertRaisesRegex(ValueError, "unknown or unselected target"):
            test_capture.diagnostic_rows([unknown], self.root, self.captured, manifest, artifacts, "server-integration", "default", "a" * 64, "b" * 64)
        wrong_source = copy.deepcopy(diagnostic)
        wrong_source["target"]["src_path"] = str(self.captured / "server/crates/waddle-server/tests/nested/main.rs")
        with self.assertRaisesRegex(ValueError, "target source"):
            test_capture.diagnostic_rows([wrong_source], self.root, self.captured, manifest, artifacts, "server-integration", "default", "a" * 64, "b" * 64)
        bad_tag = copy.deepcopy(diagnostic)
        bad_tag["message"]["message"] = "use of deprecated item W2046|UNKNOWN@L1C1"
        with self.assertRaisesRegex(ValueError, "unknown marker"):
            test_capture.diagnostic_rows([bad_tag], self.root, self.captured, manifest, artifacts, "server-integration", "default", "a" * 64, "b" * 64)

    def test_metadata_requires_retained_hash_root_and_complete_targets(self) -> None:
        with self.assertRaisesRegex(ValueError, "SHA-256"):
            test_capture.load_metadata(self.metadata_path, "0" * 64, self.root, self.captured)
        with self.assertRaisesRegex(ValueError, "workspace root"):
            test_capture.load_metadata(self.metadata_path, self.metadata_digest, self.root, pathlib.PurePosixPath("/wrong/root"))
        data = json.loads(self.metadata_path.read_text())
        data["packages"][0]["targets"] = []
        self.metadata_path.write_text(json.dumps(data) + "\n")
        digest = hashlib.sha256(self.metadata_path.read_bytes()).hexdigest()
        with self.assertRaisesRegex(ValueError, "missing metadata library"):
            test_capture.load_metadata(self.metadata_path, digest, self.root, self.captured)

    def test_metadata_rejects_missing_or_non_null_workspace_package_source(self) -> None:
        original = json.loads(self.metadata_path.read_text())
        for index in (0, 1):
            for source in ("missing", "registry"):
                data = copy.deepcopy(original)
                if source == "missing":
                    del data["packages"][index]["source"]
                else:
                    data["packages"][index]["source"] = "registry+https://example.invalid/index"
                self.metadata_path.write_text(json.dumps(data) + "\n")
                digest = hashlib.sha256(self.metadata_path.read_bytes()).hexdigest()
                with self.subTest(package=index, source=source), self.assertRaisesRegex(ValueError, "source must be present and null"):
                    test_capture.load_metadata(self.metadata_path, digest, self.root, self.captured)


class SemanticTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = pathlib.Path(self.tmp.name)
        self.rel = "server/crates/example/src/lib.rs"
        file = self.root / self.rel
        file.parent.mkdir(parents=True)
        self.source = "fn inspect() { " + "x" * 300 + " }"
        file.write_text(self.source + "\nasync fn handle() {}\n")
        self.manifest = [{"tag": "T::TYPE@L1C1"}]
        self.uses = [{"profile": profile, "tag": self.manifest[0]["tag"], "kind": "T", "file": self.rel, "line": "1", "column": "1", "source": self.source, "compiled_target_kind": "lib", "compiled_target_name": "waddle_xmpp", "compiled_target_test_mode": "false", "source_class_hint": "prod", "rw_classification": "manual-review-required"} for profile in ("default", "clustering")]
        self.coverage = [{"profile": p, "tag": self.manifest[0]["tag"], "target_declarations": "1", "typed_use_sites": "1"} for p in ("default", "clustering")]
        self.reviews = [{"source_commit": marker.SOURCE_COMMIT, "profile": r["profile"], "tag": r["tag"], "path": self.rel, "line": "1", "column": "1", "compiled_target_kind": r["compiled_target_kind"], "compiled_target_name": r["compiled_target_name"], "compiled_target_test_mode": r["compiled_target_test_mode"], "operation": "read", "classification": "authority", "rationale": "The handler reads the authoritative value.", "evidence": f"The source at {self.rel}:1 reads this value."} for r in self.uses]
        self.edge = {"source_commit": marker.SOURCE_COMMIT, "edge_id": "one", "from_path": self.rel, "from_line": "1", "to_path": self.rel, "to_line": "1", "kind": "message"}
        self.edge_review = {**self.edge, "operation": "call", "classification": "transport", "rationale": "The message transports an explicit call.", "evidence": f"The source at {self.rel}:1 identifies this edge."}

    def test_exact_site_coverage_and_classification(self) -> None:
        inventory.validate_site_reviews(self.root, self.manifest, self.uses, self.coverage, self.reviews)
        for altered in (self.reviews[:1], self.reviews + self.reviews[:1]):
            with self.assertRaises(ValueError):
                inventory.validate_site_reviews(self.root, self.manifest, self.uses, self.coverage, altered)
        bad = copy.deepcopy(self.reviews)
        bad[0]["classification"] = "manual-review-required"
        with self.assertRaisesRegex(ValueError, "classification"):
            inventory.validate_site_reviews(self.root, self.manifest, self.uses, self.coverage, bad)
        truncated = copy.deepcopy(self.uses)
        truncated[0]["source"] = truncated[0]["source"][:240]
        with self.assertRaisesRegex(ValueError, "truncated"):
            inventory.validate_site_reviews(self.root, self.manifest, truncated, self.coverage, self.reviews)
        incomplete = copy.deepcopy(self.coverage)
        incomplete[0]["typed_use_sites"] = "2"
        with self.assertRaisesRegex(ValueError, "count"):
            inventory.validate_site_reviews(self.root, self.manifest, self.uses, incomplete, self.reviews)

    def test_target_provenance_is_part_of_each_review_key(self) -> None:
        other_use = {**self.uses[0], "compiled_target_name": "waddle_server"}
        other_review = {**self.reviews[0], "compiled_target_name": "waddle_server"}
        coverage = copy.deepcopy(self.coverage)
        coverage[0]["typed_use_sites"] = "2"
        inventory.validate_site_reviews(self.root, self.manifest, self.uses + [other_use], coverage, self.reviews + [other_review])
        with self.assertRaisesRegex(ValueError, "semantic site coverage mismatch"):
            inventory.validate_site_reviews(self.root, self.manifest, self.uses + [other_use], coverage, self.reviews)
        bad = copy.deepcopy(self.uses)
        bad[0]["compiled_target_name"] = "other"
        with self.assertRaisesRegex(ValueError, "target kind or name"):
            inventory.validate_site_reviews(self.root, self.manifest, bad, self.coverage, self.reviews)

    def test_multiple_roles_require_canonical_labels_and_exclusion_is_exclusive(self) -> None:
        review = {**self.reviews[0], "operation": "read;write", "classification": "authority;derived-index"}
        inventory.validate_review(review, self.root)
        inventory.validate_review({**review, "classification": "authority;migration-glue;transport"}, self.root)
        inventory.validate_review({**review, "classification": "migration-glue;obsolete", "rationale": "This migration glue is obsolete because its old entrypoint was removed."}, self.root)
        inventory.validate_review({**review, "operation": "type-only", "classification": "excluded-type-only"}, self.root)
        inventory.validate_review({**review, "operation": "name-only", "classification": "excluded-name-only"}, self.root)
        for changed in (
            {"operation": "write;read"},
            {"operation": "read;read"},
            {"classification": "derived-index;authority"},
            {"classification": "authority;excluded-type-only"},
            {"operation": "read", "classification": "excluded-type-only"},
            {"operation": "type-only", "classification": "authority"},
            {"operation": "name-only", "classification": "authority"},
            {"operation": "read", "classification": "excluded-name-only"},
            {"classification": "migration-glue;obsolete"},
        ):
            with self.subTest(changed=changed), self.assertRaises(ValueError):
                inventory.validate_review({**review, **changed}, self.root)

    def test_use_schema_is_exact(self) -> None:
        path = self.root / "uses.tsv"
        path.write_text("profile\tbogus\ndefault\tvalue\n")
        with self.assertRaisesRegex(ValueError, "invalid columns"):
            inventory.profile_tables([path], parser.USE_FIELDS, "use-site")

    def test_indirect_candidates_require_distinct_exact_reviews(self) -> None:
        second = {**self.edge, "edge_id": "two", "kind": "callback"}
        second_review = {**self.edge_review, "edge_id": "two", "kind": "callback"}
        inventory.validate_edges(self.root, [self.edge, second], [self.edge_review, second_review])
        with self.assertRaisesRegex(ValueError, "coverage mismatch"):
            inventory.validate_edges(self.root, [self.edge, second], [self.edge_review])
        with self.assertRaisesRegex(ValueError, "duplicate indirect edge ID"):
            inventory.validate_edges(self.root, [self.edge, self.edge], [self.edge_review])
        stale = {**self.edge_review, "source_commit": "old"}
        with self.assertRaisesRegex(ValueError, "different source commit"):
            inventory.validate_edges(self.root, [self.edge], [stale])

    def test_handler_candidates_require_review_and_pinned_source(self) -> None:
        digest = hashlib.sha256((self.root / self.rel).read_bytes()).hexdigest()
        candidate = {"source_commit": marker.SOURCE_COMMIT, "path": self.rel, "line": "2", "column": "10", "actor": "UserActor", "message": "RegisterConnection", "sha256_before": digest, "cfg_test_classification": "manual-review-required", "review_status": "manual-review-required"}
        review = {key: candidate[key] for key in inventory.HANDLER_KEY}
        review.update({"source_commit": marker.SOURCE_COMMIT, "cfg_test_classification": "clustering;default", "operation": "write", "classification": "authority", "rationale": "The handler writes the resource mapping.", "evidence": f"The source at {self.rel}:2 implements this handler."})
        fingerprint = marker.handlers_fingerprint([candidate])
        inventory.validate_handlers(self.root, [candidate], [review], expected_digest=fingerprint)
        with self.assertRaisesRegex(ValueError, "coverage mismatch"):
            inventory.validate_handlers(self.root, [candidate], [], expected_digest=fingerprint)
        with self.assertRaisesRegex(ValueError, "fingerprint"):
            inventory.validate_handlers(self.root, [candidate], [review])
        for cfg in ("manual-review-required", "default;clustering", "", "unknown"):
            with self.subTest(cfg=cfg), self.assertRaisesRegex(ValueError, "handler cfg/test classification"):
                inventory.validate_handlers(self.root, [candidate], [{**review, "cfg_test_classification": cfg}], expected_digest=fingerprint)
        (self.root / self.rel).write_text(self.source + "\nasync fn handle() {} // stale\n")
        with self.assertRaisesRegex(ValueError, "hash mismatch"):
            inventory.validate_handlers(self.root, [candidate], [review], expected_digest=fingerprint)


if __name__ == "__main__":
    unittest.main()
