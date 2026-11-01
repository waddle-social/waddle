"""Anchored-path operations against a fake filesystem; no host paths are opened."""

import errno
import os
import stat
import unittest
from contextlib import ExitStack
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

import controller
import policy
import runtime
from test_lifecycle import TOKEN, PartialRoot

UID = 1000
ROOT_NAME = policy.run_name(UID, TOKEN)
COMPONENTS = ((None, "/"), (10, "run"), (11, "user"), (12, "1000"), (13, ROOT_NAME))


def directory(inode, uid=0, mode=0o755):
    return SimpleNamespace(
        st_dev=1, st_ino=inode, st_mode=stat.S_IFDIR | mode, st_uid=uid
    )


class DirectoryFS:
    def __init__(self):
        self.objects = {
            10: directory(10),
            11: directory(11),
            12: directory(12),
            13: directory(13, UID, 0o700),
            14: directory(14, UID, 0o700),
        }
        self.links = dict(zip(COMPONENTS, range(10, 15), strict=True))
        self.opened = []
        self.closed = []
        self.created = []
        self.existing_run = False

    def open(self, leaf, flags, *, dir_fd=None):
        if flags != os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC:
            raise AssertionError("directory must be anchored, nofollow and cloexec")
        key = (dir_fd, leaf)
        if key not in self.links:
            raise FileNotFoundError(errno.ENOENT, "synthetic missing component")
        fd = self.links[key]
        if not stat.S_ISDIR(self.objects[fd].st_mode):
            raise NotADirectoryError(errno.ENOTDIR, "synthetic symlink or file")
        self.opened.append((key, fd))
        return fd

    def mkdir(self, leaf, mode, *, dir_fd):
        if (dir_fd, leaf) != COMPONENTS[-1] or mode != 0o700:
            raise AssertionError("only the exclusive run root may be created")
        if self.existing_run:
            raise FileExistsError
        self.existing_run = True
        self.created.append((dir_fd, leaf, mode))

    def fstat(self, fd):
        return self.objects[fd]

    def stat(self, leaf, *, dir_fd=None, follow_symlinks=True):
        if follow_symlinks:
            raise AssertionError("identity checks must not follow symlinks")
        if (dir_fd, leaf) not in self.links:
            raise FileNotFoundError
        return self.objects[self.links[(dir_fd, leaf)]]

    def install(self, stack):
        for name, implementation in (
            ("open", self.open),
            ("mkdir", self.mkdir),
            ("fstat", self.fstat),
            ("stat", self.stat),
            ("close", self.closed.append),
        ):
            stack.enter_context(
                patch.object(runtime.os, name, side_effect=implementation)
            )
        for name in ("dup", "listdir", "fsync"):
            stack.enter_context(
                patch.object(
                    runtime.os,
                    name,
                    side_effect=AssertionError(f"unexpected downstream {name}"),
                )
            )
        stack.enter_context(patch.object(runtime.os, "getuid", return_value=UID))


class RuntimeRootTests(unittest.TestCase):
    def test_exact_chain_exclusive_creation_and_idempotent_close(self):
        fs = DirectoryFS()
        with ExitStack() as stack:
            fs.install(stack)
            root = runtime.RunRoot(UID, TOKEN, create=True)
            self.assertEqual([key for key, _fd in fs.opened], list(COMPONENTS))
            self.assertEqual(root.path, "/run/user/1000/" + ROOT_NAME)
            self.assertEqual(
                list(root.parents), ["/", "/run", "/run/user", "/run/user/1000"]
            )
            self.assertEqual(fs.created, [(13, ROOT_NAME, 0o700)])
            root.check()
            root.close()
            root.close()
        self.assertEqual(fs.closed, [14, 13, 12, 11, 10])

    def test_wrong_owner_or_mode_at_each_component_never_creates_parent(self):
        for fd in range(10, 15):
            for mutation in (
                "owner",
                "group-write",
                "other-write",
                "special",
                "too-open",
            ):
                if fd < 13 and mutation in {"special", "too-open"}:
                    continue
                with self.subTest(fd=fd, mutation=mutation):
                    fs = DirectoryFS()
                    item = fs.objects[fd]
                    if mutation == "owner":
                        item.st_uid = UID if fd < 13 else 0
                    else:
                        item.st_mode |= {
                            "group-write": 0o020,
                            "other-write": 0o002,
                            "special": 0o2000,
                            "too-open": 0o055,
                        }[mutation]
                    with ExitStack() as stack:
                        fs.install(stack)
                        expected = (
                            "runtime-parent-owner-mode" if fd < 14 else "run-root-owner"
                        )
                        with self.assertRaisesRegex(
                            policy.Rejected, "^" + expected + "$"
                        ):
                            runtime.RunRoot(UID, TOKEN, create=True)
                    self.assertEqual(
                        set(fs.closed), {opened for _key, opened in fs.opened}
                    )
                    if fd < 14:
                        self.assertEqual(fs.created, [])

    def test_missing_symlink_and_regular_file_at_each_component_fail_closed(self):
        for index, key in enumerate(COMPONENTS):
            for kind in ("missing", "symlink", "file"):
                with self.subTest(component=key, kind=kind):
                    fs = DirectoryFS()
                    if kind == "missing":
                        del fs.links[key]
                    else:
                        fs.objects[index + 10].st_mode = (
                            stat.S_IFLNK if kind == "symlink" else stat.S_IFREG
                        ) | 0o700
                    with ExitStack() as stack:
                        fs.install(stack)
                        expected = (
                            FileNotFoundError
                            if kind == "missing"
                            else NotADirectoryError
                        )
                        with self.assertRaises(expected):
                            runtime.RunRoot(UID, TOKEN)
                    self.assertEqual(
                        set(fs.closed), {opened for _key, opened in fs.opened}
                    )
                    self.assertEqual(fs.created, [])

    def test_replacement_or_mode_change_after_open_blocks_cleanup_at_each_component(
        self,
    ):
        for index, key in enumerate(COMPONENTS):
            for change in ("inode", "mode", "missing", "symlink"):
                with self.subTest(component=key, change=change):
                    fs = DirectoryFS()
                    with ExitStack() as stack:
                        fs.install(stack)
                        unlink = stack.enter_context(patch.object(runtime.os, "unlink"))
                        root = runtime.RunRoot(UID, TOKEN)
                        if change == "missing":
                            del fs.links[key]
                        elif change == "mode":
                            fs.objects[index + 10].st_mode |= 0o020
                        else:
                            fs.objects[99] = directory(
                                99, UID if index >= 3 else 0, 0o700
                            )
                            if change == "symlink":
                                fs.objects[99].st_mode = stat.S_IFLNK | 0o700
                            fs.links[key] = 99
                        if change == "missing":
                            rejection = self.assertRaises(FileNotFoundError)
                        else:
                            expected = (
                                "replaced-runtime-parent"
                                if index < 4
                                else "replaced-root"
                            )
                            rejection = self.assertRaisesRegex(
                                policy.Rejected, "^" + expected + "$"
                            )
                        with rejection:
                            root.cleanup({}, partial=True)
                        unlink.assert_not_called()
                        root.close()

    def test_collision_and_wrong_actual_uid_do_not_repair_or_reuse(self):
        fs = DirectoryFS()
        fs.existing_run = True
        with ExitStack() as stack:
            fs.install(stack)
            with self.assertRaises(FileExistsError):
                runtime.RunRoot(UID, TOKEN, create=True)
            self.assertEqual(fs.created, [])
            fs.opened.clear()
            with self.assertRaisesRegex(policy.Rejected, "runtime-actual-owner"):
                runtime.RunRoot(UID + 1, TOKEN, create=True)
            self.assertEqual(fs.opened, [])

    def test_environment_cannot_select_runtime_path(self):
        fs = DirectoryFS()
        with ExitStack() as stack:
            fs.install(stack)
            stack.enter_context(
                patch.dict(
                    os.environ,
                    {"XDG_RUNTIME_DIR": "/other", "TMPDIR": "/other", "TMP": "/other"},
                )
            )
            root = runtime.RunRoot(UID, TOKEN)
            self.assertEqual(root.path, "/run/user/1000/" + ROOT_NAME)
            root.close()

    def test_persisted_parent_identities_reject_each_replacement_before_deletion(self):
        for parent in ("/", "/run", "/run/user", "/run/user/1000"):
            root = PartialRoot()
            for name in sorted(policy.INVENTORY):
                root.create_file(name)
            root.write(
                "state.json",
                {
                    "version": 3,
                    "nonce": TOKEN,
                    "uid": UID,
                    "gid": UID,
                    "root": root.identity,
                    "parents": root.parents,
                    "inventory": root.inventory(),
                },
                None,
            )
            root.parents = dict(
                root.parents, **{parent: dict(root.parents[parent], inode=999)}
            )
            before = root.inventory()
            with (
                patch.object(controller, "RunRoot", return_value=root),
                patch.object(controller.os, "getuid", return_value=UID),
                patch.object(controller.os, "getgid", return_value=UID),
                patch.object(controller, "stop_owned") as stop,
            ):
                with self.assertRaisesRegex(policy.Rejected, "runtime-parent-identity"):
                    controller.cleanup({}, TOKEN)
                with self.assertRaisesRegex(policy.Rejected, "runtime-parent-identity"):
                    controller.state_for(root, TOKEN)
            self.assertEqual(root.inventory(), before)
            stop.assert_not_called()

    def test_persisted_parent_identity_rejects_malformed_numeric_types(self):
        current = {"/": runtime.file_identity(directory(1))}
        for key, value in (("inode", True), ("uid", False), ("device", True)):
            recorded = {"/": dict(current["/"], **{key: value})}
            with self.assertRaises(policy.Rejected):
                policy.parent_identities(recorded, current)
        for recorded in ({}, {"/": {}}, {"/": current["/"], "/other": current["/"]}):
            with self.assertRaises(policy.Rejected):
                policy.parent_identities(recorded, current)

    def test_python_and_actual_c_formatter_agree_through_maximum_uid(self):
        lines = Path("build/synthetic-paths").read_text(encoding="ascii").splitlines()
        expected = [
            policy.run_path(uid, TOKEN) + "/" + leaf
            for uid in (1, UID, 2**32 - 2)
            for leaf in ("host-file", "host.sock")
        ]
        self.assertEqual(lines, expected)
        self.assertEqual(len(expected[-1]), 86)
        self.assertTrue(all(len(line.encode("ascii")) + 1 <= 108 for line in lines))
        for uid in (0, -1, 2**32 - 1, True, "1000"):
            with self.assertRaises(policy.Rejected):
                policy.run_path(uid, TOKEN)


if __name__ == "__main__":
    unittest.main()
