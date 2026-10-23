"""farm 的离线测试: 假 adb + 真 tools/devlock, 不碰任何真设备。

  python3 -m unittest discover -s tools/farm/tests -v      (或 pytest tools/farm/tests)
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
FARM = HERE.parent / "farm.py"
SHIM = HERE.parent / "bin"
sys.path.insert(0, str(HERE.parent))
import farm  # noqa: E402

RM = "91253241019A"  # profiles.json 里的红魔: root=true, 带 ls 键
HONOR = "AVXB6R5C19000481"


class FarmCase(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="farmtest-"))
        self.state = self.tmp / "state.json"
        real = self.tmp / "realbin"
        real.mkdir()
        for t in ("adb", "hdc"):
            (real / t).write_text(f"#!/bin/sh\nexec {sys.executable} {HERE / 'fake_adb.py'} \"$@\"\n")
            (real / t).chmod(0o755)
        self.dev = {"settings": {"system:screen_brightness": "120", "global:stay_on_while_plugged_in": "0"},
                    "props": {"debug.knobs.loadop": "0"},
                    "files": {"/sys/class/qcom-battery/charging_enabled": "1"},
                    "dirs": {"/data/app/*/com.miHoYo.Yuanshen*/lib/arm64": ["libil2cpp.so", "libunity.so"]}}
        self.write_state({"online": [RM, HONOR], "dev": {RM: self.dev, HONOR: json.loads(json.dumps(self.dev))}})
        self.env_backup = dict(os.environ)
        os.environ.update({
            "FARM_HOME": str(self.tmp / "farm"),
            "FAKE_STATE": str(self.state),
            "FARM_REAL_ADB": str(real / "adb"),
            "FARM_REAL_HDC": str(real / "hdc"),
            "PATH": f"{SHIM}:{os.environ['PATH']}",
        })
        for k in ("FARM_ROLE", "FARM_AGENT", "ANDROID_SERIAL", "DEVLOCK_DIR", "DEVLOCK_LABEL"):
            os.environ.pop(k, None)

    def tearDown(self):
        os.environ.clear()
        os.environ.update(self.env_backup)
        shutil.rmtree(self.tmp, ignore_errors=True)

    # -- 工具
    def write_state(self, s):
        self.state.write_text(json.dumps(s))

    def read_state(self):
        return json.loads(self.state.read_text())

    def run_farm(self, *args, env=None, stdin=None):
        e = dict(os.environ, **(env or {}))
        return subprocess.run([sys.executable, str(FARM), *args], capture_output=True, text=True, env=e, input=stdin)

    def adb(self, *args, agent=None, role="runner"):
        """走垫片 (PATH 上的 adb), 就像 agent 在 Bash 里敲 adb 一样。"""
        e = dict(os.environ)
        if role:
            e["FARM_ROLE"] = role
        if agent:
            e["FARM_AGENT"] = agent
        return subprocess.run(["adb", *args], capture_output=True, text=True, env=e)

    def lease(self, op, serial, *labels):
        return self.run_farm("lease", op, serial, *labels)

    def begin(self, serial=RM, label="rm-runner", task=None, keys=()):
        a = ["job", "begin", serial, "--label", label] + (["--task", str(task)] if task else []) + (["--keys", *keys] if keys else [])
        return self.run_farm(*a)

    def end(self, serial=RM, label="rm-runner", task=None):
        a = ["job", "end", serial, "--label", label] + (["--task", str(task)] if task else [])
        return self.run_farm(*a)


class LeaseTest(FarmCase):
    def test_one_holder_per_device_and_queue(self):
        self.assertEqual(self.lease("acquire", RM, "a").returncode, 0)
        r = self.lease("acquire", RM, "b")
        self.assertEqual(r.returncode, 1)
        self.assertEqual(farm.lease_holder(RM), "a")
        # 另一台设备互不影响
        self.assertEqual(self.lease("acquire", HONOR, "b").returncode, 0)
        self.assertEqual(farm.lease_holder(HONOR), "b")
        self.assertEqual(self.lease("release", RM, "b").returncode, 1)  # 不是持有人不能放
        self.assertEqual(self.lease("release", RM, "a").returncode, 0)
        self.assertIsNone(farm.lease_holder(RM))
        self.assertEqual(self.lease("acquire", RM, "b").returncode, 0)  # b 在队首

    def test_stale_lease_counts_as_free(self):
        self.lease("acquire", RM, "a")
        os.environ["DEVLOCK_STALE"] = "0"
        owner = farm.lease_dir(RM) / "devlock.d" / "owner"
        os.utime(owner, (0, 0))
        self.assertIsNone(farm.lease_holder(RM))


class GuardTest(FarmCase):
    def test_non_runner_cannot_touch_device(self):
        self.lease("acquire", RM, "rm-runner")
        r = self.adb("-s", RM, "shell", "getprop", "ro.product.model", agent="rm-runner", role="research")
        self.assertEqual(r.returncode, farm.EXIT_DENY)
        self.assertIn("只有设备 runner", r.stderr)
        r = self.adb("-s", RM, "shell", "getprop", "x", role=None)  # 离线排练脚本: 什么都没设
        self.assertEqual(r.returncode, farm.EXIT_DENY)

    def test_runner_without_lease_denied(self):
        self.lease("acquire", RM, "someone-else")
        r = self.adb("-s", RM, "shell", "getprop", "x", agent="rm-runner")
        self.assertEqual(r.returncode, farm.EXIT_DENY)
        self.assertIn("someone-else", r.stderr)

    def test_no_serial_denied_but_devices_ok(self):
        self.assertEqual(self.adb("devices", role=None).returncode, 0)
        self.assertEqual(self.adb("--version", role=None).returncode, 0)
        r = self.adb("shell", "getprop", "x", agent="rm-runner")
        self.assertEqual(r.returncode, farm.EXIT_DENY)
        self.assertIn("没指定设备", r.stderr)

    def test_lease_holder_passes_through(self):
        self.lease("acquire", RM, "rm-runner")
        r = self.adb("-s", RM, "shell", "getprop", "debug.knobs.loadop", agent="rm-runner")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.strip(), "0")

    def test_destructive_commands_denied(self):
        self.lease("acquire", RM, "rm-runner")
        for cmd in (["shell", "rm", "-f", "/sdcard/x"], ["shell", "su -c 'rm /data/app/x.so'"], ["shell", "pm", "clear", "com.miHoYo.Yuanshen"],
                    ["uninstall", "com.foo"], ["shell", "settings", "delete", "global", "x"], ["shell", "echo hi > /sdcard/a.txt"],
                    ["shell", "am start -a com.x.LOGOUT"], ["kill-server"], ["shell"]):
            r = self.adb("-s", RM, *cmd, agent="rm-runner")
            self.assertEqual(r.returncode, farm.EXIT_DENY, cmd)
        calls = self.read_state().get("calls", [])
        self.assertEqual(calls, [], "被拒绝的命令一条都不该到达设备")

    def test_harmless_redirects_allowed(self):
        self.assertEqual(farm.shell_violations("cat /x 2>/dev/null; echo a >> /sdcard/log.txt; ls >/data/local/tmp/f"), [])
        self.assertEqual(farm.shell_violations("dumpsys gfxinfo pkg 2>&1 | grep frames"), [])
        self.assertTrue(farm.shell_violations("cat a > /sdcard/Download/b"))

    def test_mutation_without_job_denied(self):
        self.lease("acquire", RM, "rm-runner")
        r = self.adb("-s", RM, "shell", "settings put system screen_brightness 10", agent="rm-runner")
        self.assertEqual(r.returncode, farm.EXIT_DENY)
        self.assertIn("farm job begin", r.stderr)
        self.assertEqual(self.read_state()["dev"][RM]["settings"]["system:screen_brightness"], "120")

    def test_hdc_serial_parsing(self):
        s, sub, rest = farm.parse_argv("hdc", ["-t", "88X9K26901026869", "shell", "param", "get", "x"])
        self.assertEqual((s, sub, rest), ("hdc:88X9K26901026869", "shell", ["param", "get", "x"]))
        s, sub, _ = farm.parse_argv("hdc", ["list", "targets"])
        self.assertEqual((s, sub), (None, "list"))

    def test_mutation_parsing(self):
        keys, raw = farm.shell_mutations("su -c 'echo 0 > /sys/class/qcom-battery/charging_enabled'; setprop debug.knobs.loadop 1; "
                                         "settings put system refresh_rate_mode 1; cmd game set --downscale 0.5 com.x")
        self.assertEqual(keys, ["setting:system:refresh_rate_mode", "prop:debug.knobs.loadop", "sysfs:/sys/class/qcom-battery/charging_enabled"])
        self.assertEqual(len(raw), 1)


class JobTest(FarmCase):
    def setUp(self):
        super().setUp()
        self.lease("acquire", RM, "rm-runner")
        self.task = self.tmp / "task"

    def test_restore_after_changes(self):
        r = self.begin(task=self.task)
        self.assertEqual(r.returncode, 0, r.stderr)
        for cmd in ("settings put system screen_brightness 10", "setprop debug.knobs.loadop 1",
                    "su -c 'echo 0 > /sys/class/qcom-battery/charging_enabled'",
                    "settings put secure brand_new_but_existing 7"):
            if "brand_new" in cmd:
                st = self.read_state(); st["dev"][RM]["settings"]["secure:brand_new_but_existing"] = "3"; self.write_state(st)
            r = self.adb("-s", RM, "shell", cmd, agent="rm-runner")
            self.assertEqual(r.returncode, 0, r.stderr)
        job = farm.load_job(RM)
        self.assertEqual(job["before"]["setting:secure:brand_new_but_existing"], "3")  # 守卫补的快照
        self.assertIn("setting:secure:brand_new_but_existing", job["lazy"])
        r = self.end(task=self.task)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        dev = self.read_state()["dev"][RM]
        self.assertEqual(dev["settings"]["system:screen_brightness"], "120")
        self.assertEqual(dev["settings"]["secure:brand_new_but_existing"], "3")
        self.assertEqual(dev["props"]["debug.knobs.loadop"], "0")
        self.assertEqual(dev["files"]["/sys/class/qcom-battery/charging_enabled"], "1")
        res = json.loads((self.task / f"restore-{RM}.json").read_text())
        self.assertTrue(res["ok"])
        self.assertEqual(len(res["restored"]), 4)
        self.assertIsNone(farm.load_job(RM))

    def test_new_key_is_not_deleted_but_reported(self):
        self.begin(task=self.task)
        self.adb("-s", RM, "shell", "settings put global made_up_key 1", agent="rm-runner")
        r = self.end(task=self.task)
        self.assertEqual(r.returncode, 1)
        res = json.loads((self.task / f"restore-{RM}.json").read_text())
        self.assertFalse(res["ok"])
        self.assertEqual(res["unresolved"][0]["key"], "setting:global:made_up_key")
        calls = [" ".join(c) for c in self.read_state()["calls"]]
        self.assertFalse(any("delete" in c for c in calls))

    def test_foreign_file_in_lib_dir_is_flagged_not_deleted(self):
        self.begin(task=self.task)
        st = self.read_state()
        st["dev"][RM]["dirs"]["/data/app/*/com.miHoYo.Yuanshen*/lib/arm64"].append("libVkLayer_refknobs.so")
        self.write_state(st)  # 别的东西 (比如排练脚本) 塞进来一个文件
        r = self.end(task=self.task)
        self.assertEqual(r.returncode, 1)
        res = json.loads((self.task / f"restore-{RM}.json").read_text())
        self.assertIn("libVkLayer_refknobs.so", res["unresolved"][0]["now"])
        self.assertIn("libVkLayer_refknobs.so", self.read_state()["dev"][RM]["dirs"]["/data/app/*/com.miHoYo.Yuanshen*/lib/arm64"])

    def test_raw_mutation_needs_explanation(self):
        self.begin(task=self.task)
        r = self.adb("-s", RM, "shell", "cmd game set --downscale 0.5 com.miHoYo.Yuanshen", agent="rm-runner")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.end(task=self.task).returncode, 1)

    def test_begin_refuses_without_lease_or_with_open_job(self):
        self.assertEqual(self.begin(label="other").returncode, farm.EXIT_DENY)
        self.assertEqual(self.begin().returncode, 0)
        r = self.begin()
        self.assertEqual(r.returncode, farm.EXIT_DENY)  # 重启后的 runner 必须先收尾上一次
        self.assertIn("没收尾", r.stderr)

    def test_snapshot_files_never_overwritten(self):
        self.begin(task=self.task); self.end(task=self.task)
        self.begin(task=self.task); self.end(task=self.task)
        names = sorted(p.name for p in self.task.iterdir())
        self.assertEqual(sum(n.startswith(f"restore-{RM}.json") for n in names), 2)
        self.assertEqual(sum(n.startswith(f"snapshot-before-{RM}.json") for n in names), 2)


class HealthTest(FarmCase):
    def test_ok_hot_offline(self):
        self.assertEqual(self.run_farm("health", RM).returncode, 0)
        st = self.read_state(); st["dev"][RM]["temp10"] = 755; self.write_state(st)
        self.assertEqual(self.run_farm("health", RM).returncode, farm.EXIT_HOT)
        st["online"] = [HONOR]; self.write_state(st)
        self.assertEqual(self.run_farm("health", RM).returncode, farm.EXIT_OFFLINE)


class TaskTest(FarmCase):
    def git_repo(self):
        repo = self.tmp / "repo"
        repo.mkdir()
        g = lambda *a: subprocess.run(["git", "-C", str(repo), *a], capture_output=True, text=True, check=True).stdout.strip()
        g("init", "-q", "-b", "harness/x")
        (repo / "f").write_text("x")
        g("add", "f")
        g("-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "c")
        return repo, g("rev-parse", "HEAD")

    def test_impl_task_lifecycle(self):
        d = self.tmp / "_tmp" / "N9"
        self.assertEqual(self.run_farm("task", "new", str(d), "--id", "N9", "--role", "impl").returncode, 0)
        self.run_farm("task", "msg", str(d), "先读 BRIEF")
        self.run_farm("task", "note", str(d), "开始改代码")
        r = self.run_farm("task", "check", str(d))
        self.assertEqual(r.returncode, 1)
        self.assertIn("REPORT.md", r.stdout)
        repo, sha = self.git_repo()
        (d / "REPORT.md").write_text("# 报告\n实测: …\n")
        self.run_farm("task", "result", str(d), "--status", "done", "--commit", f"{repo}:harness/x:{sha}", "--tests", "cargo test --release|10|0")
        r = self.run_farm("task", "check", str(d))
        self.assertEqual(r.returncode, 0, r.stdout)
        self.run_farm("task", "result", str(d), "--status", "done", "--commit", f"{repo}:harness/x:deadbeef", "--tests", "x|1|2")
        r = self.run_farm("task", "check", str(d))
        self.assertIn("提交不存在", r.stdout)
        self.assertIn("测试有失败却标 done", r.stdout)
        self.assertTrue(any(p.name.startswith("RESULT.json.prev-") for p in d.iterdir()))
        h = self.run_farm("task", "handoff", str(d)).stdout
        self.assertIn("先读 BRIEF", h)
        self.assertIn("开始改代码", h)
        self.assertIn("N9", self.run_farm("task", "ls", str(self.tmp / "_tmp")).stdout)

    def test_runner_task_requires_clean_restore(self):
        d = self.tmp / "_tmp" / "R1"
        self.run_farm("task", "new", str(d), "--id", "R1", "--role", "runner", "--device", RM)
        (d / "REPORT.md").write_text("报告")
        self.run_farm("task", "result", str(d), "--status", "done")
        self.assertIn("restore-", self.run_farm("task", "check", str(d)).stdout)
        self.lease("acquire", RM, "rm-runner")
        self.begin(task=d)
        self.adb("-s", RM, "shell", "settings put system screen_brightness 5", agent="rm-runner")
        self.assertIn("没收尾", self.run_farm("task", "check", str(d)).stdout)
        self.end(task=d)
        r = self.run_farm("task", "check", str(d))
        self.assertEqual(r.returncode, 0, r.stdout)

    def test_role_device_rules(self):
        self.assertNotEqual(self.run_farm("task", "new", str(self.tmp / "a"), "--id", "a", "--role", "research", "--device", RM).returncode, 0)
        self.assertNotEqual(self.run_farm("task", "new", str(self.tmp / "b"), "--id", "b", "--role", "runner").returncode, 0)


class HookTest(FarmCase):
    def test_hook_blocks_real_binary(self):
        ev = {"tool_name": "Bash", "tool_input": {"command": "/opt/homebrew/bin/adb -s X shell ls"}}
        r = self.run_farm("hook", stdin=json.dumps(ev))
        self.assertIn('"deny"', r.stdout)
        ev["tool_input"]["command"] = "adb -s X shell ls"
        self.assertEqual(self.run_farm("hook", stdin=json.dumps(ev)).stdout.strip(), "")


if __name__ == "__main__":
    unittest.main()
