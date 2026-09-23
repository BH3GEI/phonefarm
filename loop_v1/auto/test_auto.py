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
import spin_check as SPIN       # noqa: E402


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
display.modes=fps=60,fps=120
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

    def test_fps_parser_takes_both_dumpsys_wordings(self):
        # 不同 Android 版本措辞不同, 且 120.00001 与 120 必须归成同一档
        self.assertEqual(WL._fps_list("60.000004 fps,144.00002 fps,120.00001 fps"),
                         [60, 120, 144])
        self.assertEqual(WL._fps_list("fps=60,fps=120,fps=120"), [60, 120])
        self.assertEqual(WL._fps_list(""), [])

    def test_vendor_refresh_mode_only_takes_modes_that_really_changed_fps(self):
        """厂商键的取值语义没文档, 只认探测时真把活动刷新率改掉的那几档。"""
        wl = WL.build_whitelist(WL.parse_probe(PROBE + """
setting.system.refresh_rate_mode.cur=0
setting.system.refresh_rate_mode.base_fps=120
setting.system.refresh_rate_mode.mode1_fps=60 (readback=1)
setting.system.refresh_rate_mode.mode2_fps=120 (readback=2)
setting.system.refresh_rate_mode.mode3_fps=144 (readback=9)
"""))
        spec = wl["setting.system.refresh_rate_mode"]
        # mode1 真的把 120 变成了 60 → 收; mode2 fps 没变 → 不收;
        # mode3 fps 变了但回读对不上 (写 3 读回 9) → 不收
        self.assertEqual(spec["values"], ["0", "1"])

    def test_vendor_refresh_mode_absent_when_nothing_took_effect(self):
        wl = WL.build_whitelist(WL.parse_probe(PROBE + """
setting.system.refresh_rate_mode.cur=0
setting.system.refresh_rate_mode.base_fps=120
setting.system.refresh_rate_mode.mode1_fps=120 (readback=1)
"""))
        self.assertNotIn("setting.system.refresh_rate_mode", wl)

    def test_vendor_refresh_mode_needs_real_fps_evidence(self):
        """活动刷新率读不出来 (dumpsys 措辞不同) 时一档都不许收 —— 没证据不能按下标猜。"""
        wl = WL.build_whitelist(WL.parse_probe(PROBE + """
setting.system.refresh_rate_mode.cur=0
setting.system.refresh_rate_mode.base_fps=
setting.system.refresh_rate_mode.mode1_fps= (readback=1)
setting.system.refresh_rate_mode.mode2_fps= (readback=2)
"""))
        self.assertNotIn("setting.system.refresh_rate_mode", wl)

    def test_vendor_refresh_mode_readback_must_match_exactly(self):
        """readback=10 不是 mode 1 的回读 —— 子串匹配会把它当成生效。"""
        wl = WL.build_whitelist(WL.parse_probe(PROBE + """
setting.system.refresh_rate_mode.cur=0
setting.system.refresh_rate_mode.base_fps=120
setting.system.refresh_rate_mode.mode1_fps=60 (readback=10)
"""))
        self.assertNotIn("setting.system.refresh_rate_mode", wl)

    def test_non_numeric_refresh_mode_is_skipped_not_a_crash(self):
        """厂商键回的是 auto 这种符号值时安静跳过, 不能让整条闭环崩在这儿。"""
        wl = WL.build_whitelist(WL.parse_probe(PROBE + """
setting.system.refresh_rate_mode.cur=auto
setting.system.refresh_rate_mode.base_fps=120
setting.system.refresh_rate_mode.mode1_fps=60 (readback=1)
"""))
        self.assertNotIn("setting.system.refresh_rate_mode", wl)

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
    """功耗在本机有两个实测踩出来的坑, 解析层必须如实反映, 不许美化。"""

    def test_discharging_uses_volts_times_amps_not_power_now(self):
        """本机 battery/power_now 单位有误 (读出过 777W), 所以以 |V x I| 为准。"""
        s = V.env_stats(
            "#sample_env v1\n#battery_status=Discharging\n"
            "ENV 100.0 4000000 -1500000 777000000 0 0 cpu-1-0 41000\n"
            "ENV 102.0 4000000 -1500000 777000000 0 0 gpuss-0 43500\n")
        self.assertAlmostEqual(s["power_w_mean"], 6.0, places=3)
        self.assertAlmostEqual(s["power_vi_w_mean"], 6.0, places=3)
        # power_now 原样记录, 但标成不合理, 不进任何均值
        self.assertEqual(s["power_now_w_mean"], 777.0)
        self.assertFalse(s["power_now_plausible"])
        self.assertEqual(s["soc_temp_max_c"], 43.5)
        self.assertEqual(s["n_samples"], 2)

    def test_plausible_power_now_is_still_only_recorded(self):
        s = V.env_stats("#battery_status=Discharging\n"
                        "ENV 1.0 4000000 -1500000 5000000 0 0 cpu-1-0 40000\n")
        self.assertTrue(s["power_now_plausible"])
        self.assertEqual(s["power_now_w_mean"], 5.0)
        self.assertAlmostEqual(s["power_w_mean"], 6.0, places=3)   # 仍用 V x I

    def test_charging_makes_power_unusable(self):
        """充电态: USB 输入里含给电池充电的部分, 且充电电流随电量单调衰减 ——
        那个衰减会被当成「功耗随时间下降」混进 A/B 比较。所以如实报 None。"""
        s = V.env_stats("#battery_status=Charging\n"
                        "ENV 1.0 4000000 1000000 5000000 5000000 2000000 cpu-1-0 40000\n")
        self.assertIsNone(s["power_w_mean"])
        self.assertTrue(s["battery_charging"])
        self.assertEqual(s["battery_status"], "Charging")
        self.assertIn("非放电态", s["power_usable_reason"])
        self.assertFalse(s["on_battery"])
        self.assertEqual(s["power_source"], "usb")
        # 原始量照常留下, 停充测量做好后可以回头核
        self.assertAlmostEqual(s["usb_input_w_mean"], 10.0, places=3)
        self.assertEqual(s["current_now_ua_mean"], 1000000)
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

    def test_power_out_of_verdict_drops_to_two_metrics(self):
        """功耗不计入判定时只剩两个指标, Bonferroni 相应放宽到 0.025。"""
        args = dict(self.BASE, power_available=False)
        d = V.decide({"frame_p95": _cmp(-2.0, 0.02), "fps_mean": _cmp(0.0, 1.0)}, **args)
        self.assertAlmostEqual(d["alpha_win"], 0.025, places=5)
        self.assertEqual(d["verdict"], "KEEP")

    def test_power_out_of_verdict_ignores_power_entirely(self):
        """功耗关掉时, 哪怕功耗显著变差也不算退化 —— 它根本不在判定里,
        不能拿一个量不准的数去否决一个真实的帧时改善。"""
        args = dict(self.BASE, power_available=False)
        d = V.decide({"frame_p95": _cmp(-2.0, 0.001), "fps_mean": _cmp(0.0, 1.0),
                      "power_w_mean": _cmp(5.0, 0.0001)}, **args)
        self.assertEqual(d["verdict"], "KEEP")
        self.assertEqual(d["regressions"], [])
        self.assertNotIn("power_w_mean", d["per_metric"])

    def test_rule_doc_records_why_power_is_out(self):
        r = V.rule_doc(46.0, 5, False, "功耗不计入判定, 仅记录供参考 (充电态)")
        self.assertFalse(r["power_in_verdict"])
        self.assertEqual(r["n_metrics"], 2)
        self.assertIn("充电态", r["power_note"])
        self.assertNotIn("power_w_mean", [m["id"] for m in r["metrics"]])

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


class TestSnapshotDiffClassification(unittest.TestCase):
    """留痕的判据是「这一组候选到底写过哪些项」, 不是一张写死的豁免名单。

    实测: 有一组候选只写了 DDR/LLCC 的 boost_freq, 退出时 cpu.policy0.scaling_max
    (1785600->1228800)、cpu.policy6.scaling_max、kgsl.max_pwrlevel 照样变了 ——
    红魔的厂商温控/性能管家在游戏过程中自己在改。拿写死名单豁免它们等于给自己
    开后门; 按「写没写过」分类才站得住。
    """

    def setUp(self):
        import autoloop
        self.classify = autoloop.classify_diff
        self.touched = autoloop.touched_keys
        self.keyof = autoloop.snapshot_key_of
        self.wl = WL.build_whitelist(WL.parse_probe(PROBE))

    @staticmethod
    def _sd(*pairs):
        return {"checked": True, "n_lines": 38, "identical": not pairs,
                "n_diff": len(pairs),
                "diffs": [{"line": i + 1, "before": f"{k}={a}", "after": f"{k}={b}"}
                          for i, (k, a, b) in enumerate(pairs)]}

    def test_key_mapping_matches_device_snapshot_naming(self):
        # device_snapshot.sh 用 scaling_max / governor, 不是 sysfs 的叶子名
        self.assertEqual(self.keyof("sysfs", "/sys/devices/system/cpu/cpufreq/"
                                             "policy6/scaling_max_freq"),
                         "cpu.policy6.scaling_max")
        self.assertEqual(self.keyof("sysfs", "/sys/devices/system/cpu/cpufreq/"
                                             "policy0/scaling_governor"),
                         "cpu.policy0.governor")
        self.assertEqual(self.keyof("sysfs", "/sys/class/kgsl/kgsl-3d0/max_pwrlevel"),
                         "kgsl.max_pwrlevel")
        self.assertEqual(self.keyof("sysfs", "/sys/devices/system/cpu/bus_dcvs/"
                                             "DDR/boost_freq"), "bus.DDR.boost_freq")
        self.assertEqual(self.keyof("setting", "system:refresh_rate_mode"),
                         "settings.refresh_rate_mode")

    def test_touched_keys_include_the_rollback_siblings(self):
        keys = self.touched({"cpu.policy0.scaling_governor": "performance"}, self.wl)
        # 碰 governor 会波及同 policy 的 min/max, 所以这三项都算我们的账
        self.assertEqual(keys, {"cpu.policy0.governor",
                                "cpu.policy0.scaling_max",
                                "cpu.policy0.scaling_min"})

    def test_vendor_manager_drift_on_untouched_keys_is_not_our_residue(self):
        """只写了 DDR/LLCC 的那一组, 厂商管家改的三项不该算我们留的痕。"""
        keys = self.touched({"bus.DDR.boost_freq": "5333000"}, self.wl)
        c = self.classify(self._sd(
            ("cpu.policy0.scaling_max", "1785600", "1228800"),
            ("kgsl.max_pwrlevel", "2", "0")), keys)
        self.assertTrue(c["ours_identical"])
        self.assertFalse(c["strict_identical"])        # 严格 diff 原样保留, 不藏
        self.assertEqual(len(c["environment_diffs"]), 2)

    def test_a_node_we_wrote_still_counts_as_residue(self):
        keys = self.touched({"bus.DDR.boost_freq": "5333000"}, self.wl)
        c = self.classify(self._sd(("bus.DDR.boost_freq", "0", "5333000")), keys)
        self.assertFalse(c["ours_identical"])
        self.assertEqual(c["environment_diffs"], [])

    def test_sibling_left_behind_is_caught(self):
        """切 governor 导致 scaling_max 没回来 —— 这正是它必须被抓住的那一类。"""
        keys = self.touched({"cpu.policy0.scaling_governor": "performance"}, self.wl)
        c = self.classify(self._sd(("cpu.policy0.scaling_max", "1785600", "1228800")),
                          keys)
        self.assertFalse(c["ours_identical"])

    def test_clean_snapshot_passes_both_layers(self):
        c = self.classify(self._sd(), set())
        self.assertTrue(c["ours_identical"])
        self.assertTrue(c["strict_identical"])

    def test_no_key_set_means_everything_counts(self):
        """探测阶段没有「候选写过什么」的概念, 这时一切差异都算数, 不放水。"""
        c = self.classify(self._sd(("kgsl.max_pwrlevel", "2", "0")), None)
        self.assertFalse(c["ours_identical"])


class TestHoldTest(unittest.TestCase):
    """写得进 != 守得住。厂商管家会在十几秒后把值压回去。"""

    def test_contested_nodes_stay_out_of_the_whitelist(self):
        probe = WL.parse_probe(
            "cpu.policies=0\n"
            "cpu.policy0.avail_freqs=300000 1000000 2000000\n"
            "cpu.policy0.scaling_max_freq.cur=2000000\n"
            "cpu.policy0.scaling_max_freq.writable=yes\n"
            "cpu.policy0.scaling_max_freq.effect=contested(wrote=1000000 1s=1000000 11s=1382400)\n")
        self.assertNotIn("cpu.policy0.scaling_max_freq", WL.build_whitelist(probe))

    def test_live_still_gets_in(self):
        probe = WL.parse_probe(
            "cpu.policies=0\n"
            "cpu.policy0.avail_freqs=300000 1000000 2000000\n"
            "cpu.policy0.scaling_max_freq.cur=2000000\n"
            "cpu.policy0.scaling_max_freq.writable=yes\n"
            "cpu.policy0.scaling_max_freq.effect=live\n")
        self.assertIn("cpu.policy0.scaling_max_freq", WL.build_whitelist(probe))

    def test_probe_script_reads_back_twice(self):
        src = open(os.path.join(os.path.dirname(os.path.abspath(__file__)),
                                "probe_sysparam.sh")).read()
        body = src[src.index("effect_test() {"):src.index("writable_test() {")]
        self.assertIn("contested", body)
        self.assertIn("sleep 10", body)     # 第二次回读要等够久才看得到管家的动作


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


class TestFanIsEvidenceNotAKnob(unittest.TestCase):
    """风扇转速是系统参数的一种, 但它不进自动调参白名单 —— 只当测试条件记录。"""

    def test_fan_nodes_can_never_enter_whitelist(self):
        probe = WL.parse_probe(PROBE + "\n".join([
            "fan._sys_kernel_fan_speed.writable=yes",
            "fan._sys_kernel_fan_speed.effect=live",
        ]))
        wl = WL.build_whitelist(probe)
        self.assertTrue(all("fan" not in k for k in wl))
        self.assertTrue(all("fan" not in v["path"].lower() for v in wl.values()))

    def test_fan_path_is_denied_even_if_asked_for_directly(self):
        self.assertTrue(WL._denied("/sys/kernel/fan/speed"))
        wl = WL.build_whitelist(WL.parse_probe(PROBE))
        ok, why = WL.validate_candidate({"/sys/kernel/fan/speed": "3"}, wl)
        self.assertFalse(ok)

    def test_fan_state_is_parsed_into_every_run_metrics(self):
        s = V.env_stats("#battery_status=Discharging\n"
                        "#fan_state=speed=3,fan1_input=4200\n"
                        "ENV 1.0 4000000 -1000000 NA 0 0 cpu-1-0 41000\n")
        self.assertEqual(s["fan_state"], "speed=3,fan1_input=4200")

    def test_missing_fan_readout_is_none_not_a_guess(self):
        s = V.env_stats("#battery_status=Discharging\n#fan_state=\n"
                        "ENV 1.0 4000000 -1000000 NA 0 0 cpu-1-0 41000\n")
        self.assertIsNone(s["fan_state"])


# unittest.main() 必须留在文件最末: 放在中间的话, 直接 `python3 test_auto.py`
# 会在它下面的测试类还没定义时就开跑, 那几个类一声不响地不会被收集。


class TestOnBatteryRule(unittest.TestCase):
    """on_battery 的判据与 hwcond.rs::BatteryState::on_battery 一致:
    status 不是 Charging **且** 电流确实是放电方向。两个判据都要看, 因为两个都会
    单独骗人 —— 停充之后有的内核仍写 "Not charging"; current_now 在充放平衡时会过零。"""

    def test_stop_charge_leaves_not_charging_but_still_counts(self):
        s = V.env_stats("#battery_status=Not charging\n#battery_capacity=88\n"
                        "#charge_suspended=yes\n"
                        "ENV 1.0 4000000 -1500000 NA 5000000 100000 cpu-1-0 41000\n")
        self.assertTrue(s["on_battery"])
        self.assertTrue(s["charge_suspended"])
        self.assertEqual(s["power_source"], "battery")
        self.assertAlmostEqual(s["power_w_mean"], 6.0, places=3)
        self.assertEqual(s["battery_capacity_pct"], "88")
        self.assertIn("已停充", s["power_usable_reason"])

    def test_not_charging_with_positive_current_is_not_on_battery(self):
        """status 说 Not charging 但电流是正的 = 还在往电池里灌, 不算放电态。"""
        s = V.env_stats("#battery_status=Not charging\n"
                        "ENV 1.0 4000000 771000 NA 0 0 cpu-1-0 41000\n")
        self.assertFalse(s["on_battery"])
        self.assertIsNone(s["power_w_mean"])

    def test_zero_current_is_not_on_battery(self):
        """充放平衡的瞬间电流过零 —— 判不出放电态就不算。"""
        s = V.env_stats("#battery_status=Not charging\n"
                        "ENV 1.0 4000000 0 NA 0 0 cpu-1-0 41000\n")
        self.assertFalse(s["on_battery"])
        self.assertIsNone(s["power_w_mean"])


class TestChargeCtlIsNotAKnob(unittest.TestCase):
    """停充节点是测量前提, 不是可调参数 —— 它也进不了自动调参白名单。"""

    def test_charging_nodes_never_enter_whitelist(self):
        wl = WL.build_whitelist(WL.parse_probe(PROBE + """
charge.qcom_battery_charging_enabled.writable=yes
charge.qcom_battery_charging_enabled.effect=live
"""))
        self.assertTrue(all("charg" not in k.lower() for k in wl))
        self.assertTrue(all("charg" not in v["path"].lower() for v in wl.values()))

    def test_charge_path_cannot_be_requested_directly(self):
        wl = WL.build_whitelist(WL.parse_probe(PROBE))
        ok, _ = WL.validate_candidate(
            {"/sys/class/qcom-battery/charging_enabled": "0"}, wl)
        self.assertFalse(ok)


class TestOrdinalHeuristic(unittest.TestCase):
    """「数值越大越激进」只对频率/档位成立, 对厂商枚举不成立。"""

    def setUp(self):
        self.wl = WL.build_whitelist(WL.parse_probe(PROBE + """
setting.system.refresh_rate_mode.cur=0
setting.system.refresh_rate_mode.base_fps=120
setting.system.refresh_rate_mode.mode1_fps=60 (readback=1)
setting.system.refresh_rate_mode.mode4_fps=144 (readback=4)
"""))

    def test_frequency_and_pwrlevel_are_ordinal(self):
        self.assertTrue(LLM._is_ordinal("bus.DDR.boost_freq",
                                        self.wl["bus.DDR.boost_freq"]))
        self.assertTrue(LLM._is_ordinal("gpu.min_pwrlevel",
                                        self.wl["gpu.min_pwrlevel"]))

    def test_vendor_enum_and_governor_are_not_ordinal(self):
        # refresh_rate_mode 的 1 是 60Hz 而 0 是 120Hz auto —— 数值大小无方向含义
        self.assertFalse(LLM._is_ordinal("setting.system.refresh_rate_mode",
                                         self.wl["setting.system.refresh_rate_mode"]))
        self.assertFalse(LLM._is_ordinal("cpu.policy0.scaling_governor",
                                         self.wl["cpu.policy0.scaling_governor"]))

    def test_mutator_can_still_reach_every_vendor_mode(self):
        """非序数参数要能等概率取到任何一个别的值, 不能只往一个方向走。"""
        seen = set()
        for seed in range(80):
            for c in LLM.local_mutate(self.wl, [], 3, seed=seed):
                v = c["params"].get("setting.system.refresh_rate_mode")
                if v:
                    seen.add(v)
        self.assertEqual(seen, {"1", "4"})   # 当前值 0 之外的全部可达




class TestSpinCheck(unittest.TestCase):
    """原神 7.1.0 之后手柄注入安静失效 —— 脚本跑完、有帧时序, 但视角不转。
    这组测试钉住「画面真的在动」这条判据。"""

    W, H = 64, 64          # 4096 像素: 大于抽样下限, 不会退化成只看一个点

    def _frame(self, fill):
        import struct
        head = struct.pack("<III", self.W, self.H, 1)
        return head + bytes(fill)

    def _solid(self, r, g, b):
        return self._frame([r, g, b, 255] * (self.W * self.H))

    def test_identical_frames_are_not_spinning(self):
        f = self._solid(10, 20, 30)
        v = SPIN.verdict(f, f)
        self.assertFalse(v["spinning"])
        self.assertEqual(v["moved_fraction"], 0.0)
        self.assertIn("7.1.0", v["note"])

    def test_whole_frame_change_is_spinning(self):
        v = SPIN.verdict(self._solid(10, 20, 30), self._solid(200, 210, 220))
        self.assertTrue(v["spinning"])
        self.assertEqual(v["moved_fraction"], 1.0)

    def test_tiny_animation_does_not_count_as_spinning(self):
        """静止场景里草和 UI 时钟也会动几个像素 —— 那不算视角在转。"""
        import struct
        px = [10, 20, 30, 255] * (self.W * self.H)
        head = struct.pack("<III", self.W, self.H, 1)
        a = head + bytes(px)
        px2 = list(px)
        for i in range(8):                   # 4096 个像素里只有 8 个大变 (0.2%)
            px2[i * 4:i * 4 + 4] = [250, 250, 250, 255]
        b = head + bytes(px2)
        v = SPIN.verdict(a, b)
        self.assertFalse(v["spinning"])

    def test_sub_threshold_noise_is_ignored(self):
        """编码噪声级别的浮动 (每通道 <= 16) 不算变化。"""
        v = SPIN.verdict(self._solid(100, 100, 100), self._solid(110, 108, 112))
        self.assertEqual(v["moved_fraction"], 0.0)

    def test_malformed_capture_reports_error_not_a_pass(self):
        """读不懂的帧一律判「没在转」——  宁可白停一次, 不要放过静止数据。"""
        v = SPIN.verdict(b"", b"")
        self.assertFalse(v["spinning"])
        self.assertIn("error", v)

    def test_header_length_is_derived_not_assumed(self):
        """头 12 / 16 字节两种都要认, 对不上就报错而不是瞎猜。"""
        import struct
        px = bytes([1, 2, 3, 255] * (self.W * self.H))
        w, h, p = SPIN.parse_screencap(struct.pack("<IIII", self.W, self.H, 1, 0) + px)
        self.assertEqual((w, h, len(p)), (self.W, self.H, len(px)))
        with self.assertRaises(ValueError):
            SPIN.parse_screencap(struct.pack("<III", self.W, self.H, 1) + px[:-8])



    def test_small_frames_do_not_degenerate_to_one_sample(self):
        """帧比固定步长还小时要自动加密抽样, 否则一个像素变了就等于 100% 变了。"""
        import struct
        w = h = 16                            # 256 像素, 远小于固定步长下的样本需求
        head = struct.pack("<III", w, h, 1)
        px = [10, 20, 30, 255] * (w * h)
        a = head + bytes(px)
        px2 = list(px)
        px2[0:4] = [250, 250, 250, 255]       # 256 个里只变 1 个
        frac, total = SPIN.moved_fraction(a, head + bytes(px2))
        self.assertEqual(total, 256)          # 全采, 而不是只采 4 个
        self.assertAlmostEqual(frac, 1 / 256, places=6)


class TestKnobRollbackBlastRadius(unittest.TestCase):
    """写一个节点会把兄弟节点一起改掉 —— 实测把 cpufreq 的 governor 切成
    performance 再切回 walt, scaling_max_freq 从 1785600 变成 1228800 且没回来,
    一组本来干净的数据 (p95 -7.2%) 因此作废。回滚必须覆盖整组兄弟节点。

    直接把脚本里的 siblings() 抠出来在真 sh 里跑, 不做字符串位置断言 ——
    要验的是行为, 不是源码长什么样。"""

    SH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "knob_sysparam.sh")

    @classmethod
    def setUpClass(cls):
        src = open(cls.SH).read()
        cls.fn = src[src.index("siblings() {"):src.index("read_one() {")]

    def siblings(self, path):
        import subprocess
        r = subprocess.run(["sh", "-c", self.fn + f'\nsiblings "{path}"'],
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        return r.stdout.split()

    def test_touching_any_cpufreq_node_covers_the_whole_policy(self):
        base = "/sys/devices/system/cpu/cpufreq/policy0"
        for touched in ("scaling_min_freq", "scaling_max_freq", "scaling_governor"):
            got = self.siblings(f"{base}/{touched}")
            self.assertEqual(got, [f"{base}/scaling_governor",
                                   f"{base}/scaling_max_freq",
                                   f"{base}/scaling_min_freq"],
                             f"碰 {touched} 时波及面不对")

    def test_governor_is_restored_before_min_and_max(self):
        """governor 会重设 min/max, 所以它必须排在回滚顺序最前。"""
        got = self.siblings("/sys/devices/system/cpu/cpufreq/policy6/scaling_governor")
        self.assertTrue(got[0].endswith("scaling_governor"))
        self.assertTrue(got[-1].endswith("scaling_min_freq"))

    def test_kgsl_pwrlevel_pair_is_covered_both_ways(self):
        for touched in ("min_pwrlevel", "max_pwrlevel"):
            got = self.siblings(f"/sys/class/kgsl/kgsl-3d0/{touched}")
            self.assertEqual(got, ["/sys/class/kgsl/kgsl-3d0/max_pwrlevel",
                                   "/sys/class/kgsl/kgsl-3d0/min_pwrlevel"])

    def test_unrelated_node_has_no_blast_radius(self):
        p = "/sys/devices/system/cpu/bus_dcvs/DDR/boost_freq"
        self.assertEqual(self.siblings(p), [p])

    def test_state_is_written_before_any_device_write(self):
        """回滚依据必须先落盘再动设备, 否则中途被杀就回不去了。"""
        src = open(self.SH).read()
        apply_part = src[src.index("  apply)"):src.index("  restore)")]
        self.assertLess(apply_part.index('> "$STATE"'), apply_part.index("两遍写"))


class TestTempCapIsASafetyLimit(unittest.TestCase):
    """温度上限的职责是「别把机器烤坏」, 不是「保证两臂热态一样」。"""

    def test_cap_floor_is_a_real_hardware_margin(self):
        import autoloop
        # 45C 在本机跑原神根本达不到 (基线就 55.2C), 拿它当上限每组都会作废
        self.assertGreaterEqual(autoloop.TEMP_CAP_FLOOR_C, 60.0)
        # 骁龙结温保护在 95C 上下, 上限要留足余量
        self.assertLess(autoloop.TEMP_CAP_FLOOR_C, 90.0)

    def test_margin_tolerates_ordinary_drift(self):
        """同一场连跑里 0.5C 的漂移再正常不过, 不该判成安全事故。"""
        import autoloop
        self.assertGreaterEqual(autoloop.TEMP_CAP_MARGIN_C, 5.0)


class TestClampedValuesFeedBack(unittest.TestCase):
    """探测只验过一个试写值, 不代表每个合法值都写得进去 —— 实测
    gpu.min_pwrlevel 写 0 会被内核夹到 2 (热限档位)。这条要喂回模型。"""

    def test_prompt_carries_the_clamp_warning(self):
        hist = [{"params": {"gpu.min_pwrlevel": "0"}, "verdict": "ABORT",
                 "reason": "旋钮未全部生效",
                 "clamped": ["KNOB_FAIL /sys/class/kgsl/kgsl-3d0/min_pwrlevel: "
                             "想写 0, 回读 2 (原值 17)"]}]
        p = LLM.build_prompt("- gpu.min_pwrlevel (当前 17) 可选: 0 .. 17", hist, 3)
        self.assertIn("写不进去", p)
        self.assertIn("回读 2", p)

    def test_system_prompt_tells_the_model_not_to_repeat_them(self):
        self.assertIn("被内核夹回", LLM.SYSTEM_PROMPT)


if __name__ == "__main__":
    unittest.main()
