//! capture: 原神无 UI 自动巡航截图 (SPEC_SR_LOOP Gate 2)。
//!
//! 只做"拉起游戏 → 复用 genshin 插件的巡航单步 → 抓原始 PNG → 状态过滤/冻结去重 → 落盘 + manifest"。
//! 不改插件决策 (只调用其公开的生命周期与单步接口), 不侵入游戏进程; 帧是 adb screencap 的原始字节, 不重编码。
//! "无 UI" 的落地: 只保留大世界探索态的帧 (无对白/转场/标题/竖屏弹窗), HUD 固定区域由离线切块脚本按
//! SPEC_SR_LOOP §4 的裁剪框排除。门禁驱动: 收满 --frames 张即停, --max-steps 只是安全阀。
use crate::plugins::genshin::{classify_state, hud_present, GenshinQuestAgent, GenshinState, QuestConfig};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::process::Command;
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
    if out.write_all(text.as_bytes()).is_err() || out.flush().is_err() {
        std::process::exit(0);
    }
}

fn progress(msg: &str) {
    eprintln!("[capture] {msg}");
}

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
}

const USAGE: &str = "用法: phonefarm capture --serial <设备> [--out <目录>] [--frames 200] [--max-steps N] [--settle-ms 800]\n\
      [--mode auto|navigate|dialogue] [--no-shutdown] [--ready-only] [--json]";

fn parse_args(args: &[String]) -> Result<CaptureArgs, String> {
    let mut a = CaptureArgs { serial: None, out: None, frames: 200, max_steps: 0, settle_ms: 800,
                              mode: "auto".into(), shutdown: true, json: false, ready_only: false };
    let need = |args: &[String], i: usize, name: &str| -> Result<String, String> {
        args.get(i + 1).cloned().ok_or_else(|| format!("{name} 需要一个值\n{USAGE}"))
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--serial" => { a.serial = Some(need(args, i, "--serial")?); i += 1; }
            "--out" => { a.out = Some(need(args, i, "--out")?); i += 1; }
            "--frames" => { a.frames = need(args, i, "--frames")?.parse().map_err(|_| "--frames 需为正整数")?; i += 1; }
            "--max-steps" => { a.max_steps = need(args, i, "--max-steps")?.parse().map_err(|_| "--max-steps 需为正整数")?; i += 1; }
            "--settle-ms" => { a.settle_ms = need(args, i, "--settle-ms")?.parse().map_err(|_| "--settle-ms 需为整数")?; i += 1; }
            "--mode" => { a.mode = need(args, i, "--mode")?; i += 1; }
            "--no-shutdown" => a.shutdown = false,
            "--ready-only" => a.ready_only = true,
            "--json" => a.json = true,
            other => return Err(format!("无法识别的参数 '{other}'\n{USAGE}")),
        }
        i += 1;
    }
    if a.serial.is_none() { return Err(format!("缺 --serial\n{USAGE}")); }
    if a.frames == 0 { return Err("--frames 必须 >= 1".into()); }
    // 安全阀缺省 = 目标帧数的 4 倍步数: 状态过滤与冻结去重都会消耗步数
    if a.max_steps == 0 { a.max_steps = a.frames.saturating_mul(4).max(20); }
    Ok(a)
}

/// 原始 PNG 字节 (adb exec-out screencap -p), 不经任何解码重编码
fn screencap_png(adb: &str, serial: &str) -> Option<Vec<u8>> {
    let out = Command::new(adb).args(["-s", serial, "exec-out", "screencap", "-p"]).output().ok()?;
    if out.stdout.len() < 1000 { None } else { Some(out.stdout) }
}

/// 64x36 灰度缩略图: 冻结判定用
fn thumb(img: &image::DynamicImage) -> Vec<u8> {
    img.resize_exact(64, 36, image::imageops::FilterType::Triangle).to_luma8().into_raw()
}

/// 两张缩略图的平均绝对差 (纯函数, 单测)
pub fn thumb_diff(a: &[u8], b: &[u8]) -> f32 {
    if a.is_empty() || a.len() != b.len() { return 255.0; }
    a.iter().zip(b).map(|(x, y)| (*x as i32 - *y as i32).abs() as f32).sum::<f32>() / a.len() as f32
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
    let a = match parse_args(args) {
        Ok(a) => a,
        Err(e) => { eprintln!("{e}"); return 2; }
    };
    match capture(&a) {
        Ok((report, code)) => {
            if a.json {
                emit(&format!("{}\n", serde_json::to_string_pretty(&report).unwrap_or_default()));
            } else {
                emit(&format!("capture 完成: 保留 {} 帧 / {} 步, 跳过 {}, 冻结 {}, 目录 {}\n",
                    report["frames"], report["steps"], report["skipped"], report["duplicates"], report["out"].as_str().unwrap_or("?")));
            }
            code
        }
        Err(e) => {
            if a.json {
                emit(&format!("{}\n", json!({"v": 1, "ok": false, "error": e})));
            } else {
                eprintln!("capture 失败: {e}");
            }
            2
        }
    }
}

fn capture(a: &CaptureArgs) -> Result<(Value, i32), String> {
    let t_all = Instant::now();
    let serial = a.serial.clone().unwrap();
    if serial.starts_with("hdc:") {
        return Err("capture 只支持 Android/adb 设备".into());
    }
    let adb = crate::device::locate_adb().ok_or("未找到 adb")?;
    let out_dir = match &a.out {
        Some(d) => std::path::PathBuf::from(d),
        None => std::path::PathBuf::from(format!("tasks/sr_capture_{}", chrono::Local::now().format("%Y%m%d_%H%M%S"))),
    };
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("建不了输出目录 {}: {e}", out_dir.display()))?;
    let tmp = std::env::temp_dir().join(format!("phonefarm-capture-{}", std::process::id())).to_string_lossy().to_string();
    let _ = std::fs::create_dir_all(&tmp);
    let phone = crate::device::Device::new(Some(serial.clone()), tmp);
    if !phone.health_check(8000) {
        return Err("设备无心跳".into());
    }
    let cfg = QuestConfig { mode: a.mode.clone(), serial: Some(serial.clone()), max_seconds: u64::MAX,
                            auto_choice: true, auto_shutdown: a.shutdown };
    let mut agent = GenshinQuestAgent::new(&phone, cfg);
    progress("拉起游戏并等待进入大世界 (插件 ensure_game_ready)");
    agent.ensure_game_ready()?;

    let manifest_path = out_dir.join("manifest.jsonl");
    let mut manifest = std::fs::OpenOptions::new().create(true).append(true).open(&manifest_path)
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
            let Some(png) = screencap_png(&adb, &serial) else { grab_fail += 1; continue; };
            let Ok(img) = image::load_from_memory(&png) else { grab_fail += 1; continue; };
            let (w, h) = (img.width(), img.height());
            if w < h { portrait += 1; continue; }
            let (state, _) = classify_state(&img);
            let (minimap, combat) = hud_present(&img);
            if state != GenshinState::OpenWorldExplore || !(minimap && combat) {
                progress(&format!("ready-only: 状态 {} (小地图={minimap} 技能栏={combat}), 再看", state_name(state)));
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
            progress(&format!("ready-only: 已在大世界探索态, 保留 {file} ({w}x{h}), 不走位"));
            break;
        }
    }
    while kept < a.frames && steps < a.max_steps {
        steps += 1;
        // 巡航一步: 决策完全在插件里, 这里只负责在两步之间抓帧
        if let Err(e) = agent.step() {
            progress(&format!("第{steps}步插件单步异常: {e}"));
            std::thread::sleep(Duration::from_millis(500));
        }
        std::thread::sleep(Duration::from_millis(a.settle_ms));
        let Some(png) = screencap_png(&adb, &serial) else { grab_fail += 1; continue; };
        let Ok(img) = image::load_from_memory(&png) else { grab_fail += 1; continue; };
        let (w, h) = (img.width(), img.height());
        if w < h {
            portrait += 1;
            progress(&format!("第{steps}步: 竖屏画面 {w}x{h} (系统弹窗/非游戏), 跳过"));
            continue;
        }
        let (state, _) = classify_state(&img);
        let (minimap, combat) = hud_present(&img);
        if state != GenshinState::OpenWorldExplore || !(minimap && combat) {
            let name = if state == GenshinState::OpenWorldExplore { no_hud += 1; "explore_no_hud" } else { state_name(state) };
            *skipped.entry(name).or_default() += 1;
            progress(&format!("第{steps}步: 状态 {name} (小地图={minimap} 技能栏={combat}), 跳过"));
            // 同一非探索态卡住 → 发 BACK 关弹窗 (公告/签到类界面插件的 A 键推不动), 有界且计数
            if stuck_state == Some(name) { stuck_n += 1; } else { stuck_state = Some(name); stuck_n = 1; }
            if stuck_n >= STUCK_STEPS_BACK {
                backs += 1;
                stuck_n = 0;
                progress(&format!("第{steps}步: {name} 连续 {STUCK_STEPS_BACK} 步未变, 发 BACK 键尝试关闭弹窗 (第{backs}次)"));
                phone.shell("input keyevent 4", 3000);
            }
            continue;
        }
        stuck_state = None;
        stuck_n = 0;
        let th = thumb(&img);
        let diff = thumb_diff(&prev_thumb, &th);
        if !prev_thumb.is_empty() && diff < DUP_THRESH {
            dups += 1;
            progress(&format!("第{steps}步: 与上一保留帧差 {diff:.2} < {DUP_THRESH}, 画面冻结, 跳过"));
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
        progress(&format!("第{steps}步: 保留 {file} ({w}x{h}, {} KB, 差 {diff:.1}) 进度 {kept}/{}", png.len() / 1024, a.frames));
    }
    if a.shutdown {
        progress("收尾: 复位手柄, 强停游戏并锁屏 (插件 shutdown_and_lock)");
        agent.shutdown_and_lock();
    }
    let ok = kept >= a.frames;
    let report = json!({
        "v": 1, "ok": ok, "serial": serial, "out": out_dir.to_string_lossy(), "manifest": manifest_path.to_string_lossy(),
        "frames": kept, "target": a.frames, "steps": steps, "max_steps": a.max_steps,
        "skipped": skipped.values().sum::<u32>(), "skipped_by_state": skipped, "duplicates": dups,
        "portrait": portrait, "grab_failed": grab_fail, "explore_without_hud": no_hud, "back_presses": backs, "segment_frames": SEGMENT_FRAMES, "dup_thresh": DUP_THRESH,
        "mode": a.mode, "shutdown": a.shutdown, "ready_only": a.ready_only, "wall_s": t_all.elapsed().as_secs_f64(),
    });
    let _ = std::fs::write(out_dir.join("capture.json"), serde_json::to_string_pretty(&report).unwrap_or_default());
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
        assert_eq!((a.frames, a.max_steps, a.settle_ms, a.shutdown, a.json), (200, 800, 800, true, false));
        let a = parse_args(&["--serial".into(), "X".into(), "--frames".into(), "3".into(), "--no-shutdown".into(), "--json".into()]).unwrap();
        assert_eq!((a.frames, a.max_steps, a.shutdown, a.json), (3, 20, false, true));
        assert!(!a.ready_only, "缺省不是 ready-only");
        let a = parse_args(&["--serial".into(), "X".into(), "--frames".into(), "1".into(), "--ready-only".into(), "--no-shutdown".into()]).unwrap();
        assert!(a.ready_only && !a.shutdown && a.frames == 1);
        assert!(parse_args(&[]).is_err());
        assert!(parse_args(&["--serial".into(), "X".into(), "--bogus".into()]).is_err());
    }
}
