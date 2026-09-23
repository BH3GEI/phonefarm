#!/usr/bin/env python3
"""test_auto.py — auto/ 下全部纯函数的单测。

    cd loop_v1/auto && python3 -m unittest -v

只测纯函数 (白名单构建/校验、plan 生成、功耗温度解析、判定规则、模型回包解析、
密钥解析)。涉及设备的部分不在这里测 —— 那部分由真机跑出来的 report.json 作证。
"""
from __future__ import annotations
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(
    os.path.abspath(__file__))), "tools"))

import whitelist as WL          # noqa: E402
import verdict as V             # noqa: E402
import llm as LLM               # noqa: E402


PROBE = """# probe_sysparam v1
cpu.policies=0,3
cpu.policy0.avail_freqs=300000 1000000 2000000
cpu.policy0.avail_governors=schedutil performance
cpu.policy0.scaling_min_freq.cur=300000
cpu.policy0.scaling_max_freq.cur=2000000
cpu.policy0.scaling_governor.cur=schedutil
cpu.policy0.scaling_min_freq.writable=yes
cpu.policy0.scaling_min_freq.effect=live
cpu.policy0.scaling_max_freq.writable=yes
cpu.policy0.scaling_governor.writable=yes
cpu.policy3.avail_freqs=500000 3000000
cpu.policy3.scaling_min_freq.cur=500000
cpu.policy3.scaling_min_freq.writable=yes
cpu.policy3.scaling_min_freq.effect=rejected(wrote=3000000 readback=500000)
gpu.num_pwrlevels=4
gpu.min_pwrlevel.cur=3
gpu.max_pwrlevel.cur=0
gpu.min_pwrlevel.writable=yes
gpu.min_pwrlevel.effect=live
gpu.max_pwrlevel.writable=yes
gpu.devfreq.avail_freqs=220000000 1000000000
gpu.devfreq.min_freq.cur=220000000
gpu.devfreq.min_freq.writable=no
bus.DDR.boost_freq.cur=0
bus.DDR.hw_min_freq=200000
bus.DDR.hw_max_freq=5333000
bus.DDR.avail_freqs=200000 3200000 5333000
bus.DDR.boost_freq.writable=yes
bus.DDR.boost_freq.effect=live
setting.system.peak_refresh_rate.cur=120
setting.system.min_refresh_rate.cur=60
display.modes=60.0 fps,120.0 fps
thermal.cpu-1-0=42000
"""


class TestWhitelist(unittest.TestCase):
    def setUp(self):
        self.wl = WL.build_whitelist(WL.parse_probe(PROBE))

    def test_only_writable_and_effective_nodes_get_in(self):
        self.assertIn("cpu.policy0.scaling_min_freq", self.wl)
        # 写了被内核退回 → 不进
        self.assertNotIn("cpu.policy3.scaling_min_freq", self.wl)
        # 不可写 → 不进
        self.assertNotIn("gpu.devfreq.min_freq", self.wl)

    def test_thermal_nodes_can_never_enter(self):
        """温控保护相关的东西, 哪怕探测报可写可生效也不准进白名单。"""
        probe = WL.parse_probe(PROBE + "\n".join([
            "gpu.thermal_pwrlevel.writable=yes",
            "gpu.thermal_pwrlevel.effect=live",
        ]))
        wl = WL.build_whitelist(probe)
        self.assertTrue(all("thermal" not in k for k in wl))
        self.assertTrue(all("thermal" not in v["path"] for v in wl.values()))

    def test_single_valued_params_are_dropped(self):
        """取值表只有一个值 = 没什么可调的, 不进白名单。"""
        wl = WL.build_whitelist(WL.parse_probe(
            "cpu.policies=0\n"
            "cpu.policy0.avail_freqs=1000000\n"
            "cpu.policy0.scaling_min_freq.cur=1000000\n"
            "cpu.policy0.scaling_min_freq.writable=yes\n"))
        self.assertNotIn("cpu.policy0.scaling_min_freq", wl)

    def test_refresh_rate_values_come_from_display_modes(self):
        self.assertEqual(self.wl["setting.system.peak_refresh_rate"]["values"], ["60", "120"])
        self.assertEqual(self.wl["setting.system.peak_refresh_rate"]["kind"], "setting")

    def test_bus_uses_available_frequencies(self):
        self.assertEqual(self.wl["bus.DDR.boost_freq"]["values"],
                         ["200000", "3200000", "5333000"])


class TestValidate(unittest.TestCase):
    def setUp(self):
        self.wl = WL.build_whitelist(WL.parse_probe(PROBE))

    def test_accepts_legal_candidate(self):
        ok, why = WL.validate_candidate({"bus.DDR.boost_freq": "5333000"}, self.wl)
        self.assertTrue(ok, why)

    def test_rejects_unknown_param(self):
        ok, why = WL.validate_candidate({"/sys/class/thermal/thermal_zone0/mode": "disabled"},
                                        self.wl)
        self.assertFalse(ok)
        self.assertIn("不在白名单", why)

    def test_rejects_value_outside_table(self):
        ok, why = WL.validate_candidate({"bus.DDR.boost_freq": "9999999"}, self.wl)
        self.assertFalse(ok)
        self.assertIn("不在合法取值表", why)

    def test_rejects_cpu_min_above_max(self):
        ok, why = WL.validate_candidate(
            {"cpu.policy0.scaling_min_freq": "2000000",
             "cpu.policy0.scaling_max_freq": "1000000"}, self.wl)
        self.assertFalse(ok)
        self.assertIn("超过同簇 max", why)

    def test_rejects_gpu_pwrlevel_inversion(self):
        # 0 是最快档, 所以 max_pwrlevel 必须 <= min_pwrlevel
        ok, why = WL.validate_candidate(
            {"gpu.min_pwrlevel": "0", "gpu.max_pwrlevel": "3"}, self.wl)
        self.assertFalse(ok)
        self.assertIn("必须 <=", why)

    def test_rejects_refresh_min_above_peak(self):
        ok, why = WL.validate_candidate(
            {"setting.system.min_refresh_rate": "120",
             "setting.system.peak_refresh_rate": "60"}, self.wl)
        self.assertFalse(ok)

    def test_rejects_empty(self):
        self.assertFalse(WL.validate_candidate({}, self.wl)[0])


class TestPlanText(unittest.TestCase):
    def setUp(self):
        self.wl = WL.build_whitelist(WL.parse_probe(PROBE))

    def test_plan_is_tab_separated_and_deterministic(self):
        cand = {"cpu.policy0.scaling_min_freq": "1000000",
                "cpu.policy0.scaling_max_freq": "2000000",
                "setting.system.peak_refresh_rate": "60"}
        t1 = WL.plan_text(cand, self.wl)
        t2 = WL.plan_text(dict(reversed(list(cand.items()))), self.wl)
        self.assertEqual(t1, t2)          # 同一组参数, 逐字节相同
        body = [l for l in t1.splitlines() if not l.startswith("#")]
        self.assertTrue(all(len(l.split("\t")) == 3 for l in body))
        # 放宽上限的项排在抬高下限之前, 少踩一次内核的 min<=max 夹取
        self.assertLess(t1.index("scaling_max_freq"), t1.index("scaling_min_freq"))
        self.assertIn("setting\tsystem:peak_refresh_rate\t60", t1)


class TestEnvStats(unittest.TestCase):
    def test_power_prefers_device_reported_wattage(self):
        s = V.env_stats(
            "#sample_env v1\n#battery_status=Discharging\n"
            "ENV 100.0 4000000 -1000000 5000000 0 0 cpu-1-0 41000\n"
            "ENV 102.0 4000000 -1000000 5000000 0 0 gpuss-0 43500\n")
        self.assertEqual(s["power_w_mean"], 5.0)      # 用 power_now, 不去乘 V x I
        self.assertEqual(s["soc_temp_max_c"], 43.5)
        self.assertEqual(s["n_samples"], 2)

    def test_falls_back_to_volt_times_amp(self):
        s = V.env_stats("#battery_status=Discharging\n"
                        "ENV 1.0 4000000 -1500000 NA 0 0 cpu-1-0 40000\n")
        self.assertAlmostEqual(s["power_w_mean"], 6.0, places=3)

    def test_charging_makes_power_unusable(self):
        """充电态读到的是流入功率, 量不到整机开销 —— 如实报 None, 不假装能测。"""
        s = V.env_stats("#battery_status=Charging\n"
                        "ENV 1.0 4000000 1000000 5000000 5000000 2000000 cpu-1-0 40000\n")
        self.assertIsNone(s["power_w_mean"])
        self.assertTrue(s["battery_charging"])
        self.assertEqual(s["soc_temp_max_c"], 40.0)

    def test_sentinel_temps_are_skipped(self):
        s = V.env_stats("#battery_status=Discharging\n"
                        "ENV 1.0 4000000 -1000000 NA 0 0 cpu-1-0 0\n"
                        "ENV 2.0 4000000 -1000000 NA 0 0 cpu-1-0 200000\n")
        self.assertIsNone(s["soc_temp_max_c"])


def _cmp(diff, p, a=100.0):
    return {"metric": "m", "diff_mean": diff, "diff_pct": diff / a * 100,
            "perm_p_two_sided": p, "a_mean": a, "b_mean": a + diff, "ci95": None}


class TestVerdict(unittest.TestCase):
    BASE = dict(temp_max_c=42.0, temp_cap_c=46.0, apply_ok=True,
                snapshot_identical=True, power_available=True)

    def test_keep_on_significant_improvement(self):
        d = V.decide({"frame_p95": _cmp(-2.0, 0.001),
                      "fps_mean": _cmp(0.0, 1.0),
                      "power_w_mean": _cmp(0.0, 1.0)}, **self.BASE)
        self.assertEqual(d["verdict"], "KEEP")
        self.assertEqual(d["wins"], ["frame_p95"])

    def test_bonferroni_tightens_the_win_side(self):
        """3 个指标时 alpha_win=0.0167, p=0.03 的改善还不够格。"""
        d = V.decide({"frame_p95": _cmp(-2.0, 0.03),
                      "fps_mean": _cmp(0.0, 1.0),
                      "power_w_mean": _cmp(0.0, 1.0)}, **self.BASE)
        self.assertEqual(d["verdict"], "REJECT")
        self.assertAlmostEqual(d["alpha_win"], 0.05 / 3, places=5)

    def test_regression_side_is_not_tightened(self):
        """同样的 p=0.03, 在「变差」一侧就算数 —— 两侧故意不对称。"""
        d = V.decide({"frame_p95": _cmp(-2.0, 0.001),
                      "fps_mean": _cmp(-1.0, 0.03),
                      "power_w_mean": _cmp(0.0, 1.0)}, **self.BASE)
        self.assertEqual(d["verdict"], "REJECT")
        self.assertEqual(d["regressions"], ["fps_mean"])

    def test_power_win_counts_too(self):
        d = V.decide({"frame_p95": _cmp(0.0, 1.0),
                      "fps_mean": _cmp(0.0, 1.0),
                      "power_w_mean": _cmp(-0.5, 0.005)}, **self.BASE)
        self.assertEqual(d["verdict"], "KEEP")
        self.assertEqual(d["wins"], ["power_w_mean"])

    def test_temp_over_cap_aborts(self):
        args = dict(self.BASE, temp_max_c=50.0)
        d = V.decide({"frame_p95": _cmp(-9.0, 0.0001)}, **args)
        self.assertEqual(d["verdict"], "ABORT")
        self.assertIn("超过上限", d["reason"])

    def test_residue_aborts(self):
        args = dict(self.BASE, snapshot_identical=False)
        d = V.decide({"frame_p95": _cmp(-9.0, 0.0001)}, **args)
        self.assertEqual(d["verdict"], "ABORT")

    def test_failed_apply_aborts(self):
        args = dict(self.BASE, apply_ok=False)
        d = V.decide({"frame_p95": _cmp(-9.0, 0.0001)}, **args)
        self.assertEqual(d["verdict"], "ABORT")

    def test_power_unavailable_drops_to_two_metrics(self):
        args = dict(self.BASE, power_available=False)
        d = V.decide({"frame_p95": _cmp(-2.0, 0.02), "fps_mean": _cmp(0.0, 1.0)}, **args)
        self.assertAlmostEqual(d["alpha_win"], 0.025, places=5)
        self.assertEqual(d["verdict"], "KEEP")

    def test_rule_doc_reports_reachability(self):
        # 4v4 最小可达 p = 2/C(8,4) = 0.0286, 够不着 alpha_win=0.0167
        self.assertFalse(V.rule_doc(46.0, 4, True)["reachable"])
        # 5v5 最小可达 p = 2/C(10,5) = 0.0079, 够得着
        r = V.rule_doc(46.0, 5, True)
        self.assertTrue(r["reachable"])
        self.assertAlmostEqual(r["min_reachable_p"], 2 / 252, places=6)


class TestLLMParsing(unittest.TestCase):
    def test_parses_fenced_json(self):
        out = LLM.parse_candidates(
            '好的\n```json\n[{"why":"试试总线","params":{"bus.DDR.boost_freq":"5333000"}}]\n```',
            3)
        self.assertEqual(len(out), 1)
        self.assertEqual(out[0]["params"]["bus.DDR.boost_freq"], "5333000")

    def test_values_are_stringified(self):
        out = LLM.parse_candidates('[{"why":"x","params":{"a":123}}]', 3)
        self.assertEqual(out[0]["params"]["a"], "123")

    def test_drops_malformed_items_and_caps_count(self):
        out = LLM.parse_candidates(
            '[{"params":{}}, "junk", {"why":"a","params":{"x":"1"}}, '
            '{"why":"b","params":{"y":"2"}}, {"why":"c","params":{"z":"3"}}]', 2)
        self.assertEqual(len(out), 2)

    def test_garbage_returns_empty_not_crash(self):
        for bad in ("", "没有 JSON", "[", "[not json]", "{}"):
            self.assertEqual(LLM.parse_candidates(bad, 3), [])


class TestSecrets(unittest.TestCase):
    def test_parses_export_and_quotes(self):
        s = LLM.parse_secrets('# c\nexport A=1\nB="two"\nC=\'three\'\nbad line\n')
        self.assertEqual(s, {"A": "1", "B": "two", "C": "three"})

    def test_no_shell_expansion(self):
        """配置不是脚本: $(...) 一律当普通字符, 不给命令注入留口子。"""
        s = LLM.parse_secrets("K=$(rm -rf /)")
        self.assertEqual(s["K"], "$(rm -rf /)")


class TestLocalMutate(unittest.TestCase):
    def setUp(self):
        self.wl = WL.build_whitelist(WL.parse_probe(PROBE))

    def test_produces_valid_and_distinct_candidates(self):
        out = LLM.local_mutate(self.wl, [], 3, seed=7)
        self.assertEqual(len(out), 3)
        seen = set()
        for c in out:
            ok, why = WL.validate_candidate(c["params"], self.wl)
            self.assertTrue(ok, f"{why}: {c['params']}")
            key = tuple(sorted(c["params"].items()))
            self.assertNotIn(key, seen)
            seen.add(key)

    def test_is_deterministic_for_a_given_seed(self):
        self.assertEqual(LLM.local_mutate(self.wl, [], 3, seed=11),
                         LLM.local_mutate(self.wl, [], 3, seed=11))

    def test_avoids_history(self):
        first = LLM.local_mutate(self.wl, [], 1, seed=3)
        again = LLM.local_mutate(self.wl, [{"params": first[0]["params"]}], 2, seed=3)
        self.assertNotIn(first[0]["params"], [c["params"] for c in again])


class TestPipelineEndToEnd(unittest.TestCase):
    """离线跑通「探测 → 白名单 → 挑参数 → 校验 → plan」整条链路, 不碰设备。"""

    def setUp(self):
        self.wl = WL.build_whitelist(WL.parse_probe(PROBE))

    def test_every_generated_plan_stays_inside_the_whitelist(self):
        allowed_paths = {s["path"] for s in self.wl.values()}
        for seed in range(30):
            for cand in LLM.local_mutate(self.wl, [], 3, seed=seed):
                ok, why = WL.validate_candidate(cand["params"], self.wl)
                self.assertTrue(ok, why)
                for line in WL.plan_text(cand["params"], self.wl).splitlines():
                    if line.startswith("#"):
                        continue
                    kind, path, _val = line.split("\t")
                    self.assertIn(kind, ("sysfs", "setting"))
                    self.assertIn(path, allowed_paths)
                    self.assertFalse(any(k in path.lower() for k in WL.DENY_KEYWORDS))

    def test_model_output_is_filtered_not_trusted(self):
        """模型回包里夹带越界参数时, 整组作废, 不是「剔掉违规项后凑合跑」。"""
        raw = ('[{"why":"关热保护冲一波",'
               ' "params":{"bus.DDR.boost_freq":"5333000",'
               '           "thermal.zone0.mode":"disabled"}},'
               ' {"why":"正常一组","params":{"bus.DDR.boost_freq":"3200000"}}]')
        cands = LLM.parse_candidates(raw, 4)
        accepted = [c for c in cands if WL.validate_candidate(c["params"], self.wl)[0]]
        self.assertEqual(len(accepted), 1)
        self.assertEqual(accepted[0]["params"], {"bus.DDR.boost_freq": "3200000"})


if __name__ == "__main__":
    unittest.main()
