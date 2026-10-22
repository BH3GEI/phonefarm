//! capture: 原神无 UI 自动巡航截图 (SPEC_SR_LOOP Gate 2)。
//!
//! 只做"拉起游戏 → 复用 genshin 插件的巡航单步 → 抓原始 PNG → 状态过滤/冻结去重 → 落盘 + manifest"。
//! 不改插件决策 (只调用其公开的生命周期与单步接口), 不侵入游戏进程; 帧是 adb screencap 的原始字节, 不重编码。
//! "无 UI" 的落地: 只保留大世界探索态的帧 (无对白/转场/标题/竖屏弹窗), HUD 固定区域由离线切块脚本按
//! SPEC_SR_LOOP §4 的裁剪框排除。门禁驱动: 收满 --frames 张即停, --max-steps 只是安全阀。
use crate::plugins::genshin::{
    classify_state, hud_present, GenshinQuestAgent, GenshinState, QuestConfig, QuestDevice,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::time::{Duration, Instant};

/// 冻结判定: 64x36 灰度缩略图与上一张保留帧的平均绝对差 (0..255) 低于此值视为画面冻结 (卡住/暂停)
const DUP_THRESH: f32 = 1.0;
/// 路线分段: 每 N 张保留帧记为一段, 离线切块按段做固定划分 (同段帧不跨 train/val)
const SEGMENT_FRAMES: u32 = 10;
/// 同一非探索态连续卡住这么多步 (公告/弹窗类界面, 插件的 A 键推不动) 就发一次 BACK 键 (Genshin 弹窗通用关闭键)
const STUCK_STEPS_BACK: u32 = 6;
/// --ready-only 模式下最多连续观察多少次 (每次间隔 --settle-ms) 来确认探索态
const READY_ONLY_TRIES: u32 = 8;

fn emit(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes()).and_then(|_| out.flush());
}

fn progress(msg: &str) {
    eprintln!("[capture] {msg}");
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct CaptureArgs {
    serial: Option<String>,
    out: Option<String>,
    frames: u32,
    /// 只确认已在大世界探索态 (不执行插件单步, 不走位): 探索态+HUD 即保留 1 帧并结束; 否则退回常规巡航
    ready_only: bool,
    max_steps: u32,
    settle_ms: u64,
    mode: String,
    shutdown: bool,
    json: bool,
    recover: Option<String>,
    evidence_origin: String,
    timeout_s: u64,
}

const USAGE: &str = "用法: phonefarm capture --serial <设备> [--out <目录>] [--frames 200] [--max-steps N] [--settle-ms 800]\n\
      [--mode auto|navigate|dialogue] [--no-shutdown] [--ready-only] [--json] [--timeout-s 3600]\n\
      phonefarm capture --serial <设备> --recover <session目录> [--json]\n\
      [--evidence-origin device|synthetic]";

fn parse_args(args: &[String]) -> Result<CaptureArgs, String> {
    let mut a = CaptureArgs {
        serial: None,
        out: None,
        frames: 200,
        max_steps: 0,
        settle_ms: 800,
        mode: "auto".into(),
        shutdown: true,
        json: false,
        ready_only: false,
        recover: None,
        evidence_origin: "device".into(),
        timeout_s: 3600,
    };
    let need = |args: &[String], i: usize, name: &str| -> Result<String, String> {
        args.get(i + 1)
            .cloned()
            .ok_or_else(|| format!("{name} 需要一个值\n{USAGE}"))
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--serial" => {
                a.serial = Some(need(args, i, "--serial")?);
                i += 1;
            }
            "--out" => {
                a.out = Some(need(args, i, "--out")?);
                i += 1;
            }
            "--frames" => {
                a.frames = need(args, i, "--frames")?
                    .parse()
                    .map_err(|_| "--frames 需为正整数")?;
                i += 1;
            }
            "--max-steps" => {
                a.max_steps = need(args, i, "--max-steps")?
                    .parse()
                    .map_err(|_| "--max-steps 需为正整数")?;
                i += 1;
            }
            "--settle-ms" => {
                a.settle_ms = need(args, i, "--settle-ms")?
                    .parse()
                    .map_err(|_| "--settle-ms 需为整数")?;
                i += 1;
            }
            "--mode" => {
                a.mode = need(args, i, "--mode")?;
                i += 1;
            }
            "--no-shutdown" => a.shutdown = false,
            "--ready-only" => a.ready_only = true,
            "--json" => a.json = true,
            "--recover" => {
                a.recover = Some(need(args, i, "--recover")?);
                i += 1;
            }
            "--evidence-origin" => {
                a.evidence_origin = need(args, i, "--evidence-origin")?;
                i += 1;
            }
            "--timeout-s" => {
                a.timeout_s = need(args, i, "--timeout-s")?
                    .parse()
                    .map_err(|_| "--timeout-s 需为整数")?;
                i += 1;
            }
            other => return Err(format!("无法识别的参数 '{other}'\n{USAGE}")),
        }
        i += 1;
    }
    if a.serial.is_none() {
        return Err(format!("缺 --serial\n{USAGE}"));
    }
    if a.serial
        .as_deref()
        .is_some_and(|s| s.starts_with("hdc:") || s.starts_with("ios:"))
    {
        return Err("capture 只支持 Android/adb 设备".into());
    }
    if !matches!(a.evidence_origin.as_str(), "device" | "synthetic") {
        return Err("--evidence-origin 只认 device|synthetic".into());
    }
    if !(1..=86400).contains(&a.timeout_s) {
        return Err("--timeout-s 必须为 1..86400".into());
    }
    if a.recover.is_some() && a.out.is_some() {
        return Err("--recover 使用已有session，不与 --out 混用".into());
    }
    if a.frames == 0 {
        return Err("--frames 必须 >= 1".into());
    }
    // 安全阀缺省 = 目标帧数的 4 倍步数: 状态过滤与冻结去重都会消耗步数
    if a.max_steps == 0 {
        a.max_steps = a.frames.saturating_mul(4).max(20);
    }
    Ok(a)
}

/// 64x36 灰度缩略图: 冻结判定用
fn thumb(img: &image::DynamicImage) -> Vec<u8> {
    img.resize_exact(64, 36, image::imageops::FilterType::Triangle)
        .to_luma8()
        .into_raw()
}

/// 两张缩略图的平均绝对差 (纯函数, 单测)
pub fn thumb_diff(a: &[u8], b: &[u8]) -> f32 {
    if a.is_empty() || a.len() != b.len() {
        return 255.0;
    }
    a.iter()
        .zip(b)
        .map(|(x, y)| (*x as i32 - *y as i32).abs() as f32)
        .sum::<f32>()
        / a.len() as f32
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn state_name(s: GenshinState) -> &'static str {
    match s {
        GenshinState::TitleScreen => "title",
        GenshinState::DialogueText => "dialogue",
        GenshinState::DialogueChoice => "choice",
        GenshinState::InteractionPrompt => "interaction",
        GenshinState::Climbing => "climbing",
        GenshinState::OpenWorldExplore => "explore",
        GenshinState::LoadingOrCutscene => "loading",
    }
}

pub fn run_capture(args: &[String]) -> i32 {
    let mut args = std::iter::once("capture".to_string()).chain(args.iter().cloned()).collect();
    if let Err(e) = crate::devlock::strip_lock_args(&mut args) {
        eprintln!("{e}");
        return 2;
    }
    args.remove(0);
    if args.iter().any(|s| matches!(s.as_str(), "--help" | "-h")) {
        emit(&format!("{USAGE}\n"));
        return 0;
    }
    let mut a = match parse_args(&args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let session = if let Some(dir) = &a.recover {
        crate::capture_session::Session::resume(
            std::path::Path::new(dir),
            a.serial.as_deref().unwrap(),
        )
    } else {
        crate::devlease::Lease::target(a.serial.as_deref(), "capture").and_then(|lease| {
            a.serial = Some(lease.serial.clone());
            crate::capture_session::Session::new(
                lease,
                a.out.as_deref(),
                a.shutdown,
                &a.evidence_origin,
            )
        })
    };
    let session = match session {
        Ok(s) => s,
        Err(e) => {
            emit(&format!("{}\n", json!({"v":1,"ok":false,"error":e})));
            return 2;
        }
    };
    let result = if a.recover.is_some() {
        Ok((json!({"v":1,"ok":true,"recovery_only":true}), 0))
    } else {
        a.out = Some(session.dir.join("frames").to_string_lossy().into());
        session
            .prepare()
            .and_then(|_| session.run(&a, Duration::from_secs(a.timeout_s)))
    };
    let recovery = session.finish(result.as_ref().err().map(String::as_str));
    let (mut report, mut code) =
        result.unwrap_or_else(|e| (json!({"v":1,"ok":false,"error":e}), 2));
    report["session"] = json!(session.dir);
    match recovery {
        Ok(v) => report["recovery"] = v,
        Err(e) => {
            report["recovery"] = json!({"status":"required","error":e});
            report["ok"] = json!(false);
            code = 2;
        }
    }
    let path = session.dir.join(format!(
        "report-{}-{}.json",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap()
    ));
    report["report_path"] = json!(path);
    if let Err(e) = crate::confirmation_transport::new_file(&path, report.to_string().as_bytes()) {
        eprintln!("capture report persistence failed: {e}");
        code = 2;
        report["report_error"] = json!(e);
        report["ok"] = json!(false);
    }
    if a.json {
        emit(&format!(
            "{}\n",
            serde_json::to_string_pretty(&report).unwrap()
        ));
    } else {
        emit(&format!(
            "capture: ok={}, recovery={}, session={}\n",
            report["ok"],
            report["recovery"]["status"],
            session.dir.display()
        ));
    }
    code
}

/// Internal entry: inherited worker authority and liveness must precede device work.
pub(crate) fn run_worker(args: &[String]) -> i32 {
    if let Err(e) = crate::lease_command::worker_guard() {
        eprintln!("{e}");
        return 90;
    }
    if args.len() != 1 {
        eprintln!("capture worker requires one session");
        return 90;
    }
    let dir = match std::path::Path::new(&args[0]).canonicalize() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return 90;
        }
    };
    let result: Result<(Value, i32), String> = (|| {
        let a: CaptureArgs = serde_json::from_slice(&crate::confirmation_transport::bounded(
            &dir.join("request.json"),
            65536,
        )?)
        .map_err(|e| e.to_string())?;
        let state = crate::capture_session::read_state(&dir)?;
        crate::capture_session::validate(
            &state,
            a.serial.as_deref().ok_or("missing capture serial")?,
        )?;
        if a.out.as_deref() != dir.join("frames").to_str() || state["shutdown"] != a.shutdown {
            return Err("capture request differs from session".into());
        }
        let worker = crate::capture_worker::Worker::open(&dir)?;
        let captured = capture(&a, &worker);
        let closed = worker.close();
        let captured = captured?;
        closed?;
        Ok(captured)
    })();
    let (mut value, code) = match result {
        Ok((report, code)) => (json!({"report":report,"error":null}), code),
        Err(e) => (json!({"report":null,"error":e}), 2),
    };
    value["schema"] = json!("phonefarm/capture-worker-result@1");
    value["token"] = json!(std::env::var("PHONEFARM_WORKER_TOKEN").unwrap());
    value["serial"] = json!(std::env::var("PHONEFARM_WORKER_SERIAL").unwrap());
    value["exit_code"] = json!(code);
    if let Err(e) = crate::confirmation_transport::new_file(
        &dir.join("worker-result.json"),
        value.to_string().as_bytes(),
    ) {
        eprintln!("{e}");
        return 2;
    }
    code
}

fn capture(a: &CaptureArgs, phone: &crate::capture_worker::Worker) -> Result<(Value, i32), String> {
    let t_all = Instant::now();
    let serial = a.serial.clone().unwrap();
    let out_dir = std::path::PathBuf::from(a.out.as_ref().ok_or("missing capture output")?);
    let cfg = QuestConfig {
        mode: a.mode.clone(),
        serial: Some(serial.clone()),
        max_seconds: u64::MAX,
        auto_choice: true,
        auto_shutdown: false,
    };
    let mut agent = GenshinQuestAgent::new(phone, cfg);
    progress("拉起游戏并等待进入大世界 (插件 ensure_game_ready)");
    agent.ensure_game_ready()?;
    phone.check()?;

    let manifest_path = out_dir.join("manifest.jsonl");
    let mut manifest = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&manifest_path)
        .map_err(|e| format!("打不开 manifest {}: {e}", manifest_path.display()))?;
    let mut kept = 0u32;
    let mut steps = 0u32;
    let mut dups = 0u32;
    let mut portrait = 0u32;
    let mut grab_fail = 0u32;
    let mut skipped: BTreeMap<&'static str, u32> = BTreeMap::new();
    let mut no_hud = 0u32;
    let mut backs = 0u32;
    let mut stuck_state: Option<&'static str> = None;
    let mut stuck_n = 0u32;
    let mut prev_thumb: Vec<u8> = Vec::new();
    if a.ready_only {
        // 只看不动: 连续截图确认 探索态 + 小地图 + 技能栏, 命中即保留该帧并结束; READY_ONLY_TRIES 次内未命中则退回常规巡航
        for _ in 0..READY_ONLY_TRIES {
            std::thread::sleep(Duration::from_millis(a.settle_ms));
            let png = phone.png()?;
            let Ok(img) = image::load_from_memory(&png) else {
                grab_fail += 1;
                continue;
            };
            let (w, h) = (img.width(), img.height());
            if w < h {
                portrait += 1;
                continue;
            }
            let (state, _) = classify_state(&img);
            let (minimap, combat) = hud_present(&img);
            if state != GenshinState::OpenWorldExplore || !(minimap && combat) {
                progress(&format!(
                    "ready-only: 状态 {} (小地图={minimap} 技能栏={combat}), 再看",
                    state_name(state)
                ));
                continue;
            }
            let file = format!("frame_{kept:05}.png");
            std::fs::write(out_dir.join(&file), &png).map_err(|e| format!("写不了 {file}: {e}"))?;
            let row = json!({
                "i": kept, "file": file, "ts_ms": chrono::Utc::now().timestamp_millis(), "step": steps,
                "state": state_name(state), "w": w, "h": h, "bytes": png.len(), "sha256": sha256_hex(&png),
                "segment": kept / SEGMENT_FRAMES, "diff_prev": Value::Null, "ready_only": true,
            });
            writeln!(manifest, "{row}").map_err(|e| format!("manifest 写入失败: {e}"))?;
            prev_thumb = thumb(&img);
            kept += 1;
            progress(&format!(
                "ready-only: 已在大世界探索态, 保留 {file} ({w}x{h}), 不走位"
            ));
            break;
        }
    }
    while kept < a.frames && steps < a.max_steps {
        steps += 1;
        // 巡航一步: 决策完全在插件里, 这里只负责在两步之间抓帧
        agent.step()?;
        phone.check()?;
        std::thread::sleep(Duration::from_millis(a.settle_ms));
        let png = phone.png()?;
        let Ok(img) = image::load_from_memory(&png) else {
            grab_fail += 1;
            continue;
        };
        let (w, h) = (img.width(), img.height());
        if w < h {
            portrait += 1;
            progress(&format!(
                "第{steps}步: 竖屏画面 {w}x{h} (系统弹窗/非游戏), 跳过"
            ));
            continue;
        }
        let (state, _) = classify_state(&img);
        let (minimap, combat) = hud_present(&img);
        if state != GenshinState::OpenWorldExplore || !(minimap && combat) {
            let name = if state == GenshinState::OpenWorldExplore {
                no_hud += 1;
                "explore_no_hud"
            } else {
                state_name(state)
            };
            *skipped.entry(name).or_default() += 1;
            progress(&format!(
                "第{steps}步: 状态 {name} (小地图={minimap} 技能栏={combat}), 跳过"
            ));
            // 同一非探索态卡住 → 发 BACK 关弹窗 (公告/签到类界面插件的 A 键推不动), 有界且计数
            if stuck_state == Some(name) {
                stuck_n += 1;
            } else {
                stuck_state = Some(name);
                stuck_n = 1;
            }
            if stuck_n >= STUCK_STEPS_BACK {
                backs += 1;
                stuck_n = 0;
                progress(&format!("第{steps}步: {name} 连续 {STUCK_STEPS_BACK} 步未变, 发 BACK 键尝试关闭弹窗 (第{backs}次)"));
                phone.shell_write("input keyevent 4", 3000)?;
            }
            continue;
        }
        stuck_state = None;
        stuck_n = 0;
        let th = thumb(&img);
        let diff = thumb_diff(&prev_thumb, &th);
        if !prev_thumb.is_empty() && diff < DUP_THRESH {
            dups += 1;
            progress(&format!(
                "第{steps}步: 与上一保留帧差 {diff:.2} < {DUP_THRESH}, 画面冻结, 跳过"
            ));
            continue;
        }
        let file = format!("frame_{kept:05}.png");
        std::fs::write(out_dir.join(&file), &png).map_err(|e| format!("写不了 {file}: {e}"))?;
        let row = json!({
            "i": kept, "file": file, "ts_ms": chrono::Utc::now().timestamp_millis(), "step": steps,
            "state": state_name(state), "w": w, "h": h, "bytes": png.len(), "sha256": sha256_hex(&png),
            "segment": kept / SEGMENT_FRAMES, "diff_prev": if prev_thumb.is_empty() { Value::Null } else { json!(diff) },
        });
        writeln!(manifest, "{row}").map_err(|e| format!("manifest 写入失败: {e}"))?;
        prev_thumb = th;
        kept += 1;
        progress(&format!(
            "第{steps}步: 保留 {file} ({w}x{h}, {} KB, 差 {diff:.1}) 进度 {kept}/{}",
            png.len() / 1024,
            a.frames
        ));
    }
    let ok = kept >= a.frames;
    let report = json!({
        "v": 1, "ok": ok, "serial": serial, "out": out_dir.to_string_lossy(), "manifest": manifest_path.to_string_lossy(),
        "frames": kept, "target": a.frames, "steps": steps, "max_steps": a.max_steps,
        "skipped": skipped.values().sum::<u32>(), "skipped_by_state": skipped, "duplicates": dups,
        "portrait": portrait, "grab_failed": grab_fail, "explore_without_hud": no_hud, "back_presses": backs, "segment_frames": SEGMENT_FRAMES, "dup_thresh": DUP_THRESH,
        "mode": a.mode, "shutdown": a.shutdown, "ready_only": a.ready_only, "wall_s": t_all.elapsed().as_secs_f64(),
    });
    Ok((report, if ok { 0 } else { 1 }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumb_diff_forms() {
        assert_eq!(thumb_diff(&[], &[1, 2]), 255.0);
        assert_eq!(thumb_diff(&[1, 2], &[1, 2, 3]), 255.0);
        assert_eq!(thumb_diff(&[10, 20, 30], &[10, 20, 30]), 0.0);
        assert!((thumb_diff(&[10, 20, 30], &[13, 20, 33]) - 2.0).abs() < 1e-6);
    }

    #[test]
    fn args_forms() {
        let a = parse_args(&["--serial".into(), "X".into()]).unwrap();
        assert_eq!(
            (a.frames, a.max_steps, a.settle_ms, a.shutdown, a.json),
            (200, 800, 800, true, false)
        );
        let a = parse_args(&[
            "--serial".into(),
            "X".into(),
            "--frames".into(),
            "3".into(),
            "--no-shutdown".into(),
            "--json".into(),
        ])
        .unwrap();
        assert_eq!(
            (a.frames, a.max_steps, a.shutdown, a.json),
            (3, 20, false, true)
        );
        assert!(!a.ready_only, "缺省不是 ready-only");
        let a = parse_args(&[
            "--serial".into(),
            "X".into(),
            "--frames".into(),
            "1".into(),
            "--ready-only".into(),
            "--no-shutdown".into(),
        ])
        .unwrap();
        assert!(a.ready_only && !a.shutdown && a.frames == 1);
        assert!(parse_args(&[]).is_err());
        assert!(parse_args(&["--serial".into(), "X".into(), "--bogus".into()]).is_err());
    }
}
