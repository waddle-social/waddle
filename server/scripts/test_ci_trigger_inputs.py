#!/usr/bin/env python3
"""Guard the explicit CI input lists that replaced broad globs.

cuenv derives workflow `paths:` filters from task `inputs`. It reduces every
glob to its literal directory prefix, so a glob such as
`.github/workflows/waddle-server-*.yml` widens a workflow to
`.github/workflows/**`. The inputs therefore name files and directories
explicitly. This test keeps those lists from rotting:

* every tracked file a replaced glob matches is still listed, and
* a listed path that the glob would match still exists.

It also recomputes the local Cargo path-dependency closure of the client FFI,
wasm and Linux crates and fails when a lane's declared crate list is missing a
crate in the closure, or names a crate outside it. Dev-dependencies are not
part of the closure: no lane runs these crates' tests.
"""

from pathlib import Path
import re
import subprocess
import tomllib
import unittest


ROOT = Path(__file__).resolve().parents[2]
SERVER = "server"
FFI = "server/crates/waddle-xmpp-client-ffi"
WASM = "server/crates/waddle-xmpp-client-wasm"


def tracked_files():
    output = subprocess.run(
        ["git", "ls-files", "-z"], cwd=ROOT, check=True, capture_output=True, text=True
    ).stdout
    return sorted(path for path in output.split("\0") if path)


FILES = tracked_files()


def glob_regex(pattern):
    """The input glob dialect: `*` stops at `/`, `**/` may match no directory."""
    assert not set("{}[]") & set(pattern), pattern
    out = []
    index = 0
    while index < len(pattern):
        if pattern.startswith("**/", index):
            out.append("(?:.*/)?")
            index += 3
        elif pattern.startswith("**", index):
            out.append(".*")
            index += 2
        elif pattern[index] == "*":
            out.append("[^/]*")
            index += 1
        elif pattern[index] == "?":
            out.append("[^/]")
            index += 1
        else:
            out.append(re.escape(pattern[index]))
            index += 1
    return re.compile("".join(out) + r"\Z")


def is_glob(pattern):
    return any(char in pattern for char in "*?{[")


def glob_matches(pattern, files=FILES):
    regex = glob_regex(pattern)
    return {path for path in files if regex.match(path)}


def covered(entries, files):
    """The files an input list names: a glob, a file, or a directory."""
    result = set()
    for entry in entries:
        if is_glob(entry):
            result |= glob_matches(entry, files)
        else:
            prefix = entry.rstrip("/") + "/"
            result |= {path for path in files if path == entry or path.startswith(prefix)}
    return result


def string_literals(text):
    """String literals of a balanced `[...]` that opens at the start of `text`."""
    literals = []
    depth = 0
    index = 0
    while index < len(text):
        char = text[index]
        if char == '"':
            end = index + 1
            while text[end] != '"':
                end += 2 if text[end] == "\\" else 1
            literals.append(text[index + 1 : end])
            index = end
        elif text.startswith("//", index):
            newline = text.find("\n", index)
            index = len(text) if newline < 0 else newline
        elif char == "[":
            depth += 1
        elif char == "]":
            depth -= 1
            if depth == 0:
                return literals
        index += 1
    raise AssertionError("unbalanced list")


def cue_list(path, header):
    """The string literals of the first list that follows `header` in a CUE file."""
    text = (ROOT / path).read_text()
    match = re.search(header, text)
    assert match, f"{path}: no match for {header!r}"
    start = text.index("[", match.end())
    return string_literals(text[start:])


def cue_task_inputs(path, task):
    return cue_list(path, rf"\n\t*{task}: schema\.#Task & \{{[\s\S]*?\binputs:\s*")


def workflow_paths(path):
    text = (ROOT / path).read_text()
    match = re.search(r"^    paths:\n((?:      - .*\n)+)", text, re.M)
    assert match, path
    return [line[8:].strip() for line in match.group(1).splitlines()]


# (cue file, how to read the list, frame the list is read in, replaced globs)
REPLACED_GLOBS = [
    ("server/env.cue", lambda p: cue_task_inputs(p, "checkCiDrift"), "",
     [".github/workflows/waddle-server-*.yml", "ci/contributors/*.cue",
      "scripts/integration-ci*", "scripts/*cache-benchmark*"]),
    # Read relative to the server project: server/**/env.cue.
    ("server/env.cue", lambda p: cue_task_inputs(p, "checkCiDrift"), "server/",
     ["**/env.cue"]),
    ("server/env.cue", lambda p: cue_task_inputs(p, "checkAllCiDrift"), "",
     ["**/env.cue", ".github/workflows/waddle-*.yml"]),
    ("server/env.cue", lambda p: cue_task_inputs(p, "checkRootSyncDrift"), "",
     ["ci/contributors/*.cue"]),
    ("server/env.cue", lambda p: cue_list(p, r"let _namespaceRustTaskInputs ="), "",
     ["ci/contributors/*.cue"]),
    ("server/env.cue", lambda p: cue_task_inputs(p, "checkAlertRules"), "",
     ["infrastructure/waddle.cloud/rules/mimir/*.yaml"]),
    ("server/env.cue", lambda p: cue_task_inputs(p, "checkSwitchableAlternativeProgram"), "",
     ["apps/apple/**/Tests/**"]),
    ("apps/android/env.cue", lambda p: cue_task_inputs(p, "checkCiDrift"), "",
     [".github/workflows/waddle-android-*.yml"]),
    ("apps/android/env.cue", lambda p: cue_task_inputs(p, "ciGradleContract"), "",
     [".github/workflows/waddle-android-*.yml"]),
    # The Gradle inputs also hold plain `app/**` and `core/client/src/**`, which
    # cover every module's sources; a new module must be added to them.
    ("apps/android/env.cue", lambda p: cue_list(p, r"let _gradleInputs ="), "apps/android/",
     ["**/build.gradle.kts", "**/src/**"]),
    ("apps/linux/env.cue", lambda p: cue_task_inputs(p, "checkCiDrift"), "",
     [".github/workflows/waddle-linux-*.yml"]),
    ("chat/env.cue", lambda p: cue_list(p, r"let _NativeTelemetryContractInputs ="), "",
     ["apps/android/**/build.gradle.kts"]),
]


class ExplicitInputListsTest(unittest.TestCase):
    def test_lists_equal_the_globs_they_replaced(self):
        for path, reader, frame, patterns in REPLACED_GLOBS:
            declared = reader(path)
            # Entries are read in `frame`; those outside it only add noise.
            resolved = [frame + entry for entry in declared if not entry.startswith("/")]
            named = covered(resolved, FILES)
            for pattern in patterns:
                with self.subTest(file=path, frame=frame, pattern=pattern):
                    matched = glob_matches(frame + pattern)
                    self.assertTrue(matched, "the glob matches no tracked file")
                    missing = sorted(matched - named)
                    self.assertEqual([], missing, "tracked files the old glob matched are not listed")
                    regex = glob_regex(frame + pattern)
                    stale = [
                        entry
                        for entry in resolved
                        if not is_glob(entry)
                        and regex.match(entry)
                        and entry not in FILES
                    ]
                    self.assertEqual([], stale, "listed files that are not tracked")


def crate_closure(roots):
    """Local path-dependency closure of manifest directories, dev-dependencies excluded."""
    seen = {}

    def manifest(directory):
        with (ROOT / directory / "Cargo.toml").open("rb") as source:
            return tomllib.load(source)

    def workspace_root(directory):
        current = Path(directory)
        while True:
            if (ROOT / current / "Cargo.toml").is_file() and "workspace" in manifest(current):
                return current
            if current == Path("."):
                raise AssertionError(f"{directory}: no workspace root")
            current = current.parent

    def visit(directory):
        directory = Path(directory).as_posix()
        if directory in seen:
            return
        seen[directory] = None
        document = manifest(directory)
        tables = [document.get(kind, {}) for kind in ("dependencies", "build-dependencies")]
        for target in document.get("target", {}).values():
            tables += [target.get(kind, {}) for kind in ("dependencies", "build-dependencies")]
        for table in tables:
            for name, spec in table.items():
                if not isinstance(spec, dict):
                    continue
                if "path" in spec:
                    base = Path(directory)
                    relative = spec["path"]
                elif spec.get("workspace") is True:
                    workspace = workspace_root(directory)
                    inherited = manifest(workspace)["workspace"].get("dependencies", {}).get(name)
                    if not isinstance(inherited, dict) or "path" not in inherited:
                        continue
                    base = workspace
                    relative = inherited["path"]
                else:
                    continue
                visit((base / relative).resolve().relative_to(ROOT))

    for root in roots:
        visit(root)
    return set(seen)


def crate_entries(entries):
    """Crate directories (or the whole crates tree) an input list names."""
    names = set()
    for entry in entries:
        match = re.fullmatch(r"(?:\./)?(server/crates/[^/*]+)/\*\*", entry)
        if match:
            names.add(match.group(1))
        elif entry in ("server/crates/**", "server/crates"):
            names.add("server/crates/**")
    return names


VENDOR = {"server/vendor/**"}
WORKSPACE_FILES = VENDOR | {"server/Cargo.toml", "server/Cargo.lock", "server/rust-toolchain.toml"}


def lane_lists():
    ffi_roots = [FFI]
    return {
        "android _ffiInputs": (
            cue_list("apps/android/env.cue", r"let _ffiInputs ="), ffi_roots, WORKSPACE_FILES),
        "apple pullrequest": (
            workflow_paths(".github/workflows/waddle-apple-pullrequest.yml"), ffi_roots, WORKSPACE_FILES),
        "apple default": (
            workflow_paths(".github/workflows/waddle-apple-default.yml"), ffi_roots, WORKSPACE_FILES),
        "chat _WasmSourceInputs": (
            cue_list("chat/env.cue", r"let _WasmSourceInputs ="), [WASM], WORKSPACE_FILES),
        "chat _NativeTelemetryContractInputs": (
            cue_list("chat/env.cue", r"let _NativeTelemetryContractInputs ="), [WASM, FFI], VENDOR),
        # A separate Cargo workspace: the server's [patch] tables and lockfile
        # do not apply to it.
        "linux _rustInputs": (
            cue_list("apps/linux/env.cue", r"let _rustInputs ="), ["apps/linux"], {"server/Cargo.toml"}),
    }


class ClientCrateClosureTest(unittest.TestCase):
    def test_closure_is_the_declared_crate_list(self):
        for lane, (entries, roots, required) in lane_lists().items():
            with self.subTest(lane=lane):
                closure = {
                    path for path in crate_closure(roots) if path.startswith("server/crates/")
                }
                self.assertTrue(closure, "empty closure")
                declared = crate_entries(entries)
                self.assertNotIn(
                    "server/crates/**", declared,
                    "the lane lists every server crate; list the closure instead",
                )
                self.assertEqual(
                    [], sorted(closure - declared),
                    "crates in the Cargo dependency closure are missing from the lane",
                )
                self.assertEqual(
                    [], sorted(declared - closure),
                    "the lane lists crates outside the Cargo dependency closure",
                )
                self.assertEqual(
                    [], sorted(required - set(entries)),
                    "workspace manifest, lockfile, toolchain or vendor entries were dropped",
                )

    def test_closures_exclude_dev_dependencies_and_stay_narrow(self):
        # If the closure grows to most of the workspace the narrowing no
        # longer pays for itself; revisit the lanes instead of widening here.
        workspace = {path.parent.as_posix() for path in ROOT.glob("server/crates/*/Cargo.toml")}
        for roots in ([FFI], [WASM], [WASM, FFI], ["apps/linux"]):
            with self.subTest(roots=roots):
                closure = {p for p in crate_closure(roots) if p.startswith("server/crates/")}
                self.assertLessEqual(len(closure) * 2, len(workspace))


if __name__ == "__main__":
    unittest.main()
