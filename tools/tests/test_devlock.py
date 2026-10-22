import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest


ROOT = Path(__file__).resolve().parents[2]
DEVLOCK = ROOT / "tools/devlock"
BINARY = Path(os.environ.get("PHONEFARM_TEST_BINARY", ROOT / "src/target/debug/phonefarm"))


@unittest.skipUnless(shutil.which("zsh"), "devlock requires zsh")
class DevlockBridgeTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="devlock-bridge-test-")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.env = dict(os.environ, DEVLOCK_DIR=str(self.root), DEVLOCK_STALE="1",
                        PHONEFARM_LEASE_DIR=str(self.root / "leases"))

    def call(self, *args):
        return subprocess.run(["zsh", str(DEVLOCK), *args], env=self.env,
                              capture_output=True, text=True, timeout=5)

    def stale_owner(self, text="outer since fixture\n"):
        owner = self.root / "devlock.d/owner"
        owner.parent.mkdir(exist_ok=True)
        owner.write_text(text)
        os.utime(owner, (1, 1))
        return owner

    def test_strong_records_block_missing_owner_stale_take_and_same_label_bypass(self):
        for variant in ("corrupt", "unknown", "missing-owner", "dangling-marker", "token-owner"):
            with self.subTest(variant=variant):
                directory = self.root / "devlock.d"
                shutil.rmtree(directory, ignore_errors=True)
                owner = self.stale_owner("outer since fixture token=lease-token\n" if variant == "token-owner"
                                         else "outer since fixture\n")
                marker = directory / "phonefarm-lease.json"
                if variant == "dangling-marker":
                    marker.symlink_to(directory / "missing")
                elif variant != "token-owner":
                    marker.write_text("{" if variant == "corrupt" else '{"format":999}')
                if variant == "missing-owner":
                    owner.unlink()
                before_owner = owner.read_bytes() if owner.exists() else None
                before_marker = marker.read_bytes() if marker.exists() else None
                for args in (("acquire", "other"), ("renew", "outer"), ("release", "outer")):
                    result = self.call(*args)
                    self.assertNotEqual(result.returncode, 0, (args, result.stdout, result.stderr))
                    self.assertEqual(owner.read_bytes() if owner.exists() else None, before_owner)
                    self.assertEqual(marker.read_bytes() if marker.exists() else None, before_marker)
                if owner.exists():
                    self.assertEqual(owner.stat().st_mtime, 1)
                if variant == "dangling-marker":
                    self.assertTrue(marker.is_symlink())

    def test_nested_residue_blocks_reuse_but_outer_release_preserves_recovery(self):
        owner = self.stale_owner()
        nested = self.root / "phonefarm-lease.d"
        nested.mkdir()
        marker = nested / "phonefarm-lease.json"
        marker.write_text('{"format":1,"recovery_required":true}')
        before = marker.read_bytes()
        self.assertNotEqual(self.call("acquire", "other").returncode, 0)
        self.assertNotEqual(self.call("renew", "outer").returncode, 0)
        self.assertEqual(owner.stat().st_mtime, 1)
        self.assertEqual(self.call("release", "outer").returncode, 0)
        self.assertFalse(owner.parent.exists())
        self.assertEqual(marker.read_bytes(), before)
        self.assertNotEqual(self.call("acquire", "other").returncode, 0)
        marker.unlink()  # Even an empty leftover directory still blocks old acquisition.
        self.assertNotEqual(self.call("acquire", "other").returncode, 0)
        nested.rmdir()
        self.assertEqual(self.call("acquire", "other").returncode, 0)
        self.assertEqual(self.call("renew", "other").returncode, 0)
        self.assertEqual(self.call("release", "other").returncode, 0)

    @unittest.skipUnless(BINARY.is_file(), "set PHONEFARM_TEST_BINARY to an existing signed test build")
    def test_real_nested_lease_allows_child_and_outer_ancestor_renew_only(self):
        ready, finish = self.root / "ready.json", self.root / "finish"
        command = """
import json, pathlib, subprocess, sys, time
devlock, ready, finish = sys.argv[1:]
r = subprocess.run(['zsh', devlock, 'renew', 'outer'], capture_output=True, text=True)
pathlib.Path(ready).write_text(json.dumps({'code':r.returncode,'out':r.stdout,'err':r.stderr}))
while not pathlib.Path(finish).exists():
    time.sleep(0.02)
"""
        log = self.root / "run.log"
        with log.open("w") as output:
            process = subprocess.Popen(
                ["zsh", str(DEVLOCK), "run", "outer", "--", str(BINARY), "lease", "--serial", "fixture-device",
                 "--timeout-ms", "500", "--", sys.executable, "-c", command, str(DEVLOCK), str(ready), str(finish)],
                env=self.env, stdout=output, stderr=subprocess.STDOUT, text=True)
        try:
            deadline = time.monotonic() + 5
            while not ready.exists() and process.poll() is None and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertTrue(ready.exists(), "isolated lease did not start its local child")
            self.assertEqual(json.loads(ready.read_text())["code"], 0, ready.read_text())
            marker = self.root / "phonefarm-lease.d/phonefarm-lease.json"
            before = marker.read_bytes()
            # Like the outer gate heartbeat, this caller is an ancestor of the lease owner.
            renewed = self.call("renew", "outer")
            self.assertEqual(renewed.returncode, 0, renewed.stdout + renewed.stderr)
            unrelated = subprocess.run(
                [sys.executable, "-c", "import subprocess,sys; sys.exit(subprocess.call(sys.argv[1:]))",
                 "zsh", str(DEVLOCK), "renew", "outer"], env=self.env, capture_output=True, text=True, timeout=5)
            self.assertNotEqual(unrelated.returncode, 0, unrelated.stdout + unrelated.stderr)
            self.assertEqual(marker.read_bytes(), before)
            self.assertNotEqual(self.call("acquire", "other").returncode, 0)
        finally:
            finish.touch()
            process.wait(timeout=5)
        self.assertEqual(process.returncode, 0, log.read_text())
        self.assertFalse((self.root / "phonefarm-lease.d").exists())
        self.assertFalse((self.root / "devlock.d").exists())


if __name__ == "__main__":
    unittest.main()
