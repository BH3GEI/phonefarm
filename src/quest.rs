//! 原神剧情推进与任务跑图 Agent (Genshin Story & Quest Agent)
//!
//! 专为原神及同类 3D ARPG 设计的高性能轻量级自主任务助手：
//! - 剧情快速推进 (Dialogue Fast-Forward): 实时识别对话状态与分支选项 (◆)，极速下发 A 键推进
//! - 目标交互与拾取 (Auto Interact): 侦测视野内 NPC / 机关 / 掉落物的交互提示并自动下发 X 键交互
//! - 任务巡航与视角校准 (Quest Navigation): 侦测视野中任务目标标记并引导视角与奔跑 (LS + B)
//! - 防卡死与攀爬保护 (Stamina & Climbing Guard): 监测体力消耗与攀爬状态，防摔防卡墙

use std::thread::sleep;
use std::time::{Duration, Instant};
use image::{DynamicImage, GenericImageView};
use crate::device::Device;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenshinState {
    /// 对话文本播放中 (右下角技能栏隐藏，底部字幕播放)
    DialogueText,
    /// 对话分支选项出现 (右侧出现选项卡片 ◆)
    DialogueChoice,
    /// 视野内出现交互提示 (NPC / 采集物 / 机关，出现 X 键提示)
    InteractionPrompt,
    /// 角色处于攀爬状态 (黄色体力圆弧出现)
    Climbing,
    /// 大世界正常探索 / 跑图状态 (右下角技能栏正常)
    OpenWorldExplore,
    /// 黑屏 / 过场 CG / 加载中
    LoadingOrCutscene,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct QuestConfig {
    pub mode: String,          // "auto" | "dialogue" | "interact" | "navigate"
    pub serial: Option<String>,
    pub max_seconds: u64,      // 运行上限秒数 (默认 1800 秒 = 30 分钟)
    pub auto_choice: bool,     // 遇到分支选项是否自动按 A 选择
}

impl Default for QuestConfig {
    fn default() -> Self {
        Self {
            mode: "auto".to_string(),
            serial: None,
            max_seconds: 1800,
            auto_choice: true,
        }
    }
}

pub struct GenshinQuestAgent<'a> {
    device: &'a Device,
    cfg: QuestConfig,
    last_climb: Option<Instant>,
    consecutive_dialogue_ticks: u32,
}

impl<'a> GenshinQuestAgent<'a> {
    pub fn new(device: &'a Device, cfg: QuestConfig) -> Self {
        Self {
            device,
            cfg,
            last_climb: None,
            consecutive_dialogue_ticks: 0,
        }
    }

    /// 识别当前画面的游戏状态
    pub fn detect_state(&self, img: &DynamicImage) -> (GenshinState, Option<f32>) {
        let (w, h) = img.dimensions();
        if w < 100 || h < 100 {
            return (GenshinState::OpenWorldExplore, None);
        }

        // 1. 检查是否整体黑屏或过场 (平均亮度极低)
        let mut sample_sum: u64 = 0;
        let mut sample_count: u64 = 0;
        for sx in (w / 4..3 * w / 4).step_by(32) {
            for sy in (h / 4..3 * h / 4).step_by(32) {
                let p = img.get_pixel(sx, sy);
                sample_sum += (p[0] as u64 + p[1] as u64 + p[2] as u64) / 3;
                sample_count += 1;
            }
        }
        let avg_luma = sample_sum / sample_count.max(1);
        if avg_luma < 10 {
            return (GenshinState::LoadingOrCutscene, None);
        }

        // 2. 检查右下角技能栏高对比度像素 (战斗状态指示器)
        // 范围: x 82%~98%, y 72%~98%
        let mut combat_white = 0;
        let mut combat_total = 0;
        let x_start = (w as f32 * 0.82) as u32;
        let x_end = (w as f32 * 0.98) as u32;
        let y_start = (h as f32 * 0.72) as u32;
        let y_end = (h as f32 * 0.98) as u32;
        for x in (x_start..x_end).step_by(10) {
            for y in (y_start..y_end).step_by(10) {
                let p = img.get_pixel(x, y);
                combat_total += 1;
                if p[0] > 200 && p[1] > 200 && p[2] > 200 {
                    combat_white += 1;
                }
            }
        }
        let combat_ratio = combat_white as f32 / combat_total.max(1) as f32;
        let combat_ui_active = combat_ratio > 0.005;

        // 3. 检查角色身旁黄色攀爬体力槽
        // 范围: x 52%~65%, y 38%~62%
        let mut yellow_count = 0;
        let mut stamina_total = 0;
        let s_x_start = (w as f32 * 0.52) as u32;
        let s_x_end = (w as f32 * 0.65) as u32;
        let s_y_start = (h as f32 * 0.38) as u32;
        let s_y_end = (h as f32 * 0.62) as u32;
        for x in (s_x_start..s_x_end).step_by(6) {
            for y in (s_y_start..s_y_end).step_by(6) {
                let p = img.get_pixel(x, y);
                stamina_total += 1;
                if p[0] > 180 && p[1] > 150 && p[2] < 90 {
                    yellow_count += 1;
                }
            }
        }
        let stamina_ratio = yellow_count as f32 / stamina_total.max(1) as f32;
        if stamina_ratio > 0.006 {
            return (GenshinState::Climbing, None);
        }

        // 4. 检查右侧对话分支选项卡片 (白底或发光菱形)
        // 范围: x 60%~85%, y 35%~75%
        let mut choice_box_pixels = 0;
        let mut choice_total = 0;
        let c_x_start = (w as f32 * 0.60) as u32;
        let c_x_end = (w as f32 * 0.85) as u32;
        let c_y_start = (h as f32 * 0.35) as u32;
        let c_y_end = (h as f32 * 0.75) as u32;
        for x in (c_x_start..c_x_end).step_by(8) {
            for y in (c_y_start..c_y_end).step_by(8) {
                let p = img.get_pixel(x, y);
                choice_total += 1;
                // 对话卡片浅色半透明背景与金色图标
                if (p[0] > 220 && p[1] > 220 && p[2] > 220) || (p[0] > 200 && p[1] > 180 && p[2] < 100) {
                    choice_box_pixels += 1;
                }
            }
        }
        let choice_ratio = choice_box_pixels as f32 / choice_total.max(1) as f32;

        // 若无战斗技能栏，说明进入了剧情/对话界面
        if !combat_ui_active {
            if choice_ratio > 0.025 {
                return (GenshinState::DialogueChoice, None);
            } else {
                return (GenshinState::DialogueText, None);
            }
        }

        // 5. 处于大世界探索时，检查中央偏右交互提示 (X 键 / NPC 对话)
        // 范围: x 56%~72%, y 42%~60%
        let mut interact_bright = 0;
        let mut interact_total = 0;
        let i_x_start = (w as f32 * 0.56) as u32;
        let i_x_end = (w as f32 * 0.72) as u32;
        let i_y_start = (h as f32 * 0.42) as u32;
        let i_y_end = (h as f32 * 0.60) as u32;
        for x in (i_x_start..i_x_end).step_by(6) {
            for y in (i_y_start..i_y_end).step_by(6) {
                let p = img.get_pixel(x, y);
                interact_total += 1;
                if p[0] > 235 && p[1] > 235 && p[2] > 235 {
                    interact_bright += 1;
                }
            }
        }
        let interact_ratio = interact_bright as f32 / interact_total.max(1) as f32;
        if interact_ratio > 0.0035 {
            return (GenshinState::InteractionPrompt, None);
        }

        // 6. 查找任务目标标记的水平位置 (Cyan / Gold 菱形)
        let mut marker_x: Option<f32> = None;
        let m_y_start = (h as f32 * 0.15) as u32;
        let m_y_end = (h as f32 * 0.55) as u32;
        let mut max_cyan_score = 0;
        let mut best_x = 0;
        for x in (100..w - 100).step_by(12) {
            let mut col_cyan = 0;
            for y in (m_y_start..m_y_end).step_by(8) {
                let p = img.get_pixel(x, y);
                // 蓝绿色指引标: R<120, G>180, B>220
                if p[0] < 120 && p[1] > 180 && p[2] > 220 {
                    col_cyan += 1;
                }
            }
            if col_cyan > max_cyan_score && col_cyan >= 3 {
                max_cyan_score = col_cyan;
                best_x = x;
            }
        }
        if max_cyan_score >= 3 {
            marker_x = Some(best_x as f32 / w as f32);
        }

        (GenshinState::OpenWorldExplore, marker_x)
    }

    /// 执行一轮决策与动作注入
    pub fn step(&mut self) -> Result<(), String> {
        let Some(img) = self.device.screen_image() else {
            sleep(Duration::from_millis(500));
            return Ok(());
        };

        let (state, marker_x) = self.detect_state(&img);
        match state {
            GenshinState::DialogueText => {
                self.consecutive_dialogue_ticks += 1;
                println!("  [剧情模式] 正在播放对话字幕 (连续第 {} 帧) → 注入按键 A 快速推进", self.consecutive_dialogue_ticks);
                self.device.gamepad_press("a", 120)?;
                sleep(Duration::from_millis(250));
            }
            GenshinState::DialogueChoice => {
                self.consecutive_dialogue_ticks += 1;
                println!("  [剧情模式] 检测到对话分支选项 (◆) → 注入按键 A 确认选中分支");
                self.device.gamepad_press("a", 150)?;
                sleep(Duration::from_millis(400));
            }
            GenshinState::InteractionPrompt => {
                self.consecutive_dialogue_ticks = 0;
                println!("  [交互模式] 检测到 NPC / 机关可交互提示 → 注入按键 X 触发对话/调查");
                self.device.gamepad_press("x", 150)?;
                sleep(Duration::from_millis(600));
            }
            GenshinState::Climbing => {
                println!("  [状态保护] 角色正处于攀爬消耗体力中 → 下发按键 B 脱离爬墙");
                let now = Instant::now();
                if let Some(prev) = self.last_climb {
                    if now.duration_since(prev).as_secs() >= 3 {
                        // 攀爬卡死超过3秒，主动松手
                        self.device.gamepad_press("b", 150)?;
                        self.device.gamepad_stick("left", 0.0, 1.0, 400)?;
                        self.last_climb = None;
                    }
                } else {
                    self.last_climb = Some(now);
                }
                sleep(Duration::from_millis(400));
            }
            GenshinState::OpenWorldExplore => {
                self.consecutive_dialogue_ticks = 0;
                self.last_climb = None;

                if let Some(mx) = marker_x {
                    // 视野中检测到任务指引标
                    if mx < 0.44 {
                        println!("  [跑图导航] 任务目标偏左 ({:.1}%) → 右摇杆向左微调视角", mx * 100.0);
                        self.device.gamepad_stick("right", -0.65, 0.0, 250)?;
                    } else if mx > 0.56 {
                        println!("  [跑图导航] 任务目标偏右 ({:.1}%) → 右摇杆向右微调视角", mx * 100.0);
                        self.device.gamepad_stick("right", 0.65, 0.0, 250)?;
                    } else {
                        println!("  [跑图导航] 目标正居中 ({:.1}%) → 向前奔跑推进 (LS + B)", mx * 100.0);
                        self.device.gamepad_stick("left", 0.0, -1.0, 1500)?;
                        self.device.gamepad_press("b", 120)?;
                    }
                } else {
                    // 未直接看到任务点，平稳防烧屏巡航并搜索交互
                    println!("  [大世界巡航] 保持大世界走位与环视侦测");
                    self.device.gamepad_wander(3500)?;
                    self.device.gamepad_press("x", 120)?; // 盲交互周围掉落物
                }
            }
            GenshinState::LoadingOrCutscene => {
                println!("  [场景转场] 画面暗转或加载中 → 等待场景就绪");
                sleep(Duration::from_millis(1000));
            }
        }

        Ok(())
    }

    /// 持续运行主循环
    pub fn run_loop(&mut self) -> Result<(), String> {
        let start = Instant::now();
        println!("════ 启动原神剧情过关与跑图 Agent (Genshin Quest Agent) ════");
        println!("模式: {} | 最大运行时长: {}s", self.cfg.mode, self.cfg.max_seconds);
        println!("特性: 零Token消耗本地高频视觉感知 + 原生手柄低延迟注入");
        println!("────────────────────────────────────────────────────────────");

        let mut step_count = 0;
        while start.elapsed().as_secs() < self.cfg.max_seconds {
            step_count += 1;
            print!("[步数 #{:03}]", step_count);
            if let Err(e) = self.step() {
                eprintln!("  [警告: 单步执行异常: {e}]");
                sleep(Duration::from_millis(500));
            }
        }

        println!("────────────────────────────────────────────────────────────");
        println!("[完成] 原神任务/剧情 Agent 运行完成 (共执行 {} 步, 耗时 {:.1}s)", step_count, start.elapsed().as_secs_f32());
        self.device.gamepad_reset()?;
        Ok(())
    }
}
