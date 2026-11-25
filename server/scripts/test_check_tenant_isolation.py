#!/usr/bin/env python3
"""Unit tests for check-tenant-isolation.py."""
from __future__ import annotations

import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("check-tenant-isolation.py")
spec = importlib.util.spec_from_file_location("check_tenant_isolation", SCRIPT)
check = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = check
spec.loader.exec_module(check)


class TenantIsolationGuardTests(unittest.TestCase):
    def test_detects_hardcoded_local_and_room_observation_waddle_id(self):
        src = """
        fn prod() {
            let a = WaddleId::try_new("local").unwrap();
            let b = WaddleId::try_new("room-observation").unwrap();
            // WaddleId::try_new("local")
            let c = WaddleId::try_new("acme.example").unwrap();
        }
        #[cfg(test)]
        mod tests {
            fn ok_in_test() {
                let _ = WaddleId::try_new("local");
            }
        }
        """
        self.assertEqual(check.find_hardcoded_local_waddle(src), [3, 4])

    def test_detects_unscoped_store_and_registry_ctors(self):
        src = """
        fn prod() {
            let r = RoomRegistryActor::new("muc.example".into(), secret);
            let c = ConnectionRegistry::new();
            let cmd = CommandRegistry::new();
            let ok = ConnectionRegistry::for_tenant(tenant_id);
            // ConnectionRegistry::new()
        }
        #[cfg(test)]
        mod tests {
            fn ok_in_test() {
                let _ = ConnectionRegistry::new();
            }
        }
        """
        self.assertEqual(check.find_unscoped_store_ctors(src), [3, 4, 5])

    def test_detects_global_barejid_static(self):
        src = """
        static BAD_LOCKS: LazyLock<Mutex<HashMap<BareJid, Arc<Mutex<()>>>>> = todo!();
        static BAD_DASH: LazyLock<DashMap<BareJid, u32>> = todo!();
        static GOOD_LOCKS: LazyLock<Mutex<HashMap<TenantKey<BareJid>, Arc<Mutex<()>>>>> = todo!();
        """
        self.assertEqual(check.find_global_barejid_statics(src), [2, 3])

    def test_detects_unbounded_wasm_memories_and_missing_epoch_interruption(self):
        bad_host = "let l = wasmtime::StoreLimitsBuilder::new().build();"
        self.assertEqual(
            check.find_unbounded_wasm_memories(bad_host, Path("crates/waddle-extensions/src/runtime/other.rs")),
            [1],
        )
        good_host = """
        pub(super) struct AggregateStoreLimits {}
        impl AggregateStoreLimits {
            fn new() { let _ = wasmtime::StoreLimitsBuilder::new().build(); }
        }
        """
        self.assertEqual(
            check.find_unbounded_wasm_memories(good_host, Path("crates/waddle-extensions/src/runtime/host_state.rs")),
            [],
        )
        bad_loader = "fn try_new() { config.consume_fuel(true); }"
        self.assertEqual(
            check.find_unbounded_wasm_memories(bad_loader, Path("crates/waddle-extensions/src/runtime/loader.rs")),
            [1],
        )
        good_loader = "fn try_new() { config.epoch_interruption(true); }"
        self.assertEqual(
            check.find_unbounded_wasm_memories(good_loader, Path("crates/waddle-extensions/src/runtime/loader.rs")),
            [],
        )

    def test_allowlist_exact_count_enforcement(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "crates/waddle-server/src/db").mkdir(parents=True)
            (root / "crates/waddle-extensions/src/runtime").mkdir(parents=True)
            (root / "crates/waddle-xmpp/src").mkdir(parents=True)
            (root / "scripts").mkdir(parents=True)

            (root / "crates/waddle-extensions/src/runtime/loader.rs").write_text(
                "fn try_new() { config.epoch_interruption(true); }\n",
                encoding="utf-8",
            )
            (root / "crates/waddle-server/src/db/legacy.rs").write_text(
                "fn f() { let _ = ConnectionRegistry::new(); }\n",
                encoding="utf-8",
            )
            allow = root / "scripts/tenant-isolation-allowlist.txt"
            allow.write_text("", encoding="utf-8")

            # Without allowlist entry -> fails.
            _, errs = check.check_repo(root)
            self.assertEqual(len(errs), 1)

            # With exact count 1 -> passes.
            allow.write_text(
                "crates/waddle-server/src/db/legacy.rs unscoped-store-ctor 1\n",
                encoding="utf-8",
            )
            _, errs = check.check_repo(root)
            self.assertEqual(errs, [])

            # With stale count 2 -> fails.
            allow.write_text(
                "crates/waddle-server/src/db/legacy.rs unscoped-store-ctor 2\n",
                encoding="utf-8",
            )
            _, errs = check.check_repo(root)
            self.assertEqual(len(errs), 1)


if __name__ == "__main__":
    unittest.main()
