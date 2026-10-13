//! preflight: 开测前的环境不变量核查（可选自动修复）。
//!
//! 这些不变量以前散在 eval / selftest / 人肉脚本里：pair 采集被打断后风扇开了一夜、
//! selftest 前几臂风扇开着导致整组功耗作废重跑。收敛成一条命令，每臂开测前跑一遍
//! （eval 自己的风扇守卫照旧，这条命令是给 agent / 脚本 / 人用的总核查）。
//!
//!   phonefarm preflight [--serial X] [--fix] [--json] [--temp-cap 48]
//!
//! 查什么（owner 政策: 主动风扇常关——噪音；测量纪律: 手机必须静音）:
//!   fan      /sys/kernel/fan/fan_enable == 0（机器没有风扇节点记 na，不算不过）
//!   volume   媒体音量 == 0
//!   thermal  SoC 最高温 < --temp-cap（修不了只能等冷 → 不过）
//!   charging 只报不查（待机就该充电；停充是 eval 每臂自己的事）
//!   screen   只报不查（前台应用/亮屏）
//!
//! 退出码: 0 全过（或 --fix 修好了）；2 有不过；3 设备不在线或判不出是哪台。

use crate::device::{self, Device};
use serde::Serialize;

#[derive(Serialize)]
struct Check {
    name: &'static str,
    /// pass / fail / fixed / info / na
    status: String,
    detail: String,
}

#[derive(Serialize)]
struct Out {
    serial: String,
    ok: bool,
    checks: Vec<Check>,
}

fn pass(name: &'static str, detail: impl Into<String>) -> Check {
    Check { name, status: "pass".into(), detail: detail.into() }
}
fn fail(name: &'static str, detail: impl Into<String>) -> Check {
    Check { name, status: "fail".into(), detail: detail.into() }
}
fn info(name: &'static str, detail: impl Into<String>) -> Check {
    Check { name, status: "info".into(), detail: detail.into() }
}

const USAGE: &str = "phonefarm preflight [--serial X] [--fix] [--json] [--temp-cap 48]
开测前的环境不变量核查: 风扇常关(owner 政策) / 媒体静音 / SoC 温度 / 充电与前台(只报)。
--fix 自动修能修的(关风扇、音量置 0)。退出码 0 全过 / 2 有不过 / 3 判不出设备。";

pub fn run_preflight(args: &[String]) -> i32 {
    let mut serial: Option<String> = None;
    let mut fix = false;
    let mut json = false;
    let mut temp_cap = 48.0f64;
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--serial" => serial = it.next().cloned(),
            "--fix" => fix = true,
            "--json" => json = true,
            "--temp-cap" => {
                temp_cap = it.next().and_then(|v| v.parse().ok()).unwrap_or(48.0);
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return 0;
            }
            other => {
                eprintln!("preflight: 不认识的参数 {other}\n{USAGE}");
                return 2;
            }
        }
    }

    let Some(serial) = device::resolve_serial(serial.as_deref()) else {
        eprintln!("preflight: 判不出用哪台设备（零台/多台在线，--serial 指定）");
        return 3;
    };
    // run_timeout 把 stdout 写进 <tmp>/_cmd.out —— 目录不存在就读不回任何东西
    let tmpd = std::env::temp_dir().join("phonefarm-preflight");
    let _ = std::fs::create_dir_all(&tmpd);
    let tmp = tmpd.display().to_string();
    let phone = Device::new(Some(serial.clone()), tmp);

    // 锁在别人手里 → 轻量档: 只 cat sysfs（风扇/温度/充电），不拉 cmd/dumpsys 进程——
    // 对面可能正在量帧时，多一个进程就是多一帧 jank（与 fleet 同一条纪律）。
    let lock_dir = std::env::var("DEVLOCK_DIR").unwrap_or_else(|_| "/private/tmp/claude-501".into());
    let light = crate::fleet::read_lock(std::path::Path::new(&lock_dir)).is_some_and(|l| {
        !l.stale
            && l.held_by.as_ref().is_some_and(|h| {
                std::env::var("DEVLOCK_LABEL").ok().as_deref() != Some(h.as_str())
            })
    });

    let mut checks: Vec<Check> = Vec::new();

    // ── fan：owner 政策常关（噪音；且风扇自耗电进功耗读数）──
    let fan_node = "/sys/kernel/fan/fan_enable";
    let fan = phone.shell(&format!("cat {fan_node} 2>/dev/null || su -c 'cat {fan_node}'"), 8000);
    let fan = fan.trim();
    if fan.is_empty() {
        checks.push(Check { name: "fan", status: "na".into(), detail: "读不到风扇节点（这台机器没有风扇，或要 root）".into() });
    } else if fan == "0" {
        checks.push(pass("fan", "风扇关着"));
    } else if fix {
        phone.shell(&format!("su -c 'echo 0 > {fan_node}'"), 8000);
        let after = phone.shell(&format!("cat {fan_node}"), 8000);
        if after.trim() == "0" {
            checks.push(Check { name: "fan", status: "fixed".into(), detail: format!("风扇曾是 {fan} 档开着，已关") });
        } else {
            checks.push(fail("fan", format!("风扇 {fan}，写 0 没生效")));
        }
    } else {
        checks.push(fail("fan", format!("风扇开着（fan_enable={fan}）——--fix 关掉")));
    }

    // ── volume：测量纪律静音（锁在别人手里时不拉 cmd 进程，跳过记 na）──
    if light {
        checks.push(Check { name: "volume", status: "na".into(), detail: "设备锁在别人手里，不拉进程打扰测量，跳过".into() });
    } else {
        let v = phone.shell("cmd media_session volume --stream 3 --get", 8000);
        let vol = v
            .split("volume is ")
            .nth(1)
            .and_then(|s| s.split_whitespace().next())
            .and_then(|s| s.parse::<i64>().ok());
        match vol {
            Some(0) => checks.push(pass("volume", "媒体音量 0")),
            Some(n) if fix => {
                phone.shell("cmd audio set-volume 3 0", 8000);
                let v2 = phone.shell("cmd media_session volume --stream 3 --get", 8000);
                let ok = v2.split("volume is ").nth(1).and_then(|s| s.split_whitespace().next()) == Some("0");
                checks.push(if ok {
                    Check { name: "volume", status: "fixed".into(), detail: format!("媒体音量曾是 {n}，已静音") }
                } else {
                    fail("volume", format!("媒体音量 {n}，置 0 没生效"))
                });
            }
            Some(n) => checks.push(fail("volume", format!("媒体音量 {n}（测量必须静音）——--fix 置 0"))),
            None => checks.push(Check { name: "volume", status: "na".into(), detail: "读不到媒体音量".into() }),
        }
    }

    // ── thermal：SoC 结温（修不了，只能等冷）。cpu-hw-trip-* 这类区报的是跳变阈值
    //    (105°C) 不是温度，口径与 fleet 一致走 hwcond::soc_max_c（按前缀 + 跳哨兵值）──
    let zones = phone.shell(
        "for z in /sys/class/thermal/thermal_zone*; do echo \"$(cat $z/type 2>/dev/null) $(cat $z/temp 2>/dev/null)\"; done",
        8000,
    );
    let zones = crate::hwcond::parse_thermal(&zones);
    match crate::hwcond::soc_max_c(&zones) {
        None => checks.push(Check { name: "thermal", status: "na".into(), detail: "读不到 SoC 温度区".into() }),
        Some((ty, c)) => checks.push(if c < temp_cap {
            pass("thermal", format!("SoC 最高 {c:.1}°C ({ty}) < {temp_cap:.0}°C"))
        } else {
            fail("thermal", format!("SoC 最高 {c:.1}°C ({ty}) ≥ 上限 {temp_cap:.0}°C，先等冷"))
        }),
    }

    // ── charging / screen：只报不查（screen 要 dumpsys，轻量档跳过）──
    let bat = phone.shell("cat /sys/class/power_supply/battery/status 2>/dev/null", 8000);
    let ce = phone.shell("su -c 'cat /sys/class/qcom-battery/charging_enabled' 2>/dev/null", 8000);
    checks.push(info(
        "charging",
        format!("电池 {}，charging_enabled={}", bat.trim(), ce.trim()),
    ));
    if light {
        checks.push(Check { name: "screen", status: "na".into(), detail: "设备锁在别人手里，不拉 dumpsys 打扰测量，跳过".into() });
    } else {
        let focus = phone.shell("dumpsys window 2>/dev/null | grep -m1 mCurrentFocus", 8000);
        let focus = focus.trim().rsplit(['/', '}']).nth(1).unwrap_or("?").to_string();
        checks.push(info("screen", format!("前台 {focus}")));
    }

    let ok = checks.iter().all(|c| c.status != "fail");
    let out = Out { serial, ok, checks };
    if json {
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
    } else {
        for c in &out.checks {
            let mark = match c.status.as_str() {
                "pass" => "✓",
                "fixed" => "✓(已修)",
                "fail" => "✗",
                "na" => "—",
                _ => "·",
            };
            println!("{mark} {:<9} {}", c.name, c.detail);
        }
        println!("preflight: {}", if ok { "全过，可以开测" } else { "有不过——先处理（--fix 能修风扇/音量）" });
    }
    if ok { 0 } else { 2 }
}
