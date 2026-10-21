//! 插帧验收判据: 插出来的中间帧「看不出问题」= 与同一时刻的原生渲染帧看起来一样。
//!
//! 输入: 原生按 2 倍帧率渲染的连续帧 N0 N1 N2 … (偶数帧 = 插帧算子的输入, 奇数帧 = 真值中间帧),
//! 和插帧算子对每一对 (N2i, N2i+2) 插出来的 Ci。降画质换省电不算增益, 所以判据只认「跟原生一样」。
//!
//! 每张插帧 (前帧 P、真值 T、后帧 N、候选 C) 查五项, 都跟原生自己的数比, 不跟固定的全帧 PSNR 比:
//! 1. **静止块逐块保真** (HUD、文字、不动的背景): 用 P/T/N 两两块 PSNR 都 ≥ 45 dB 挑出静止块。
//!    原生连续帧在这些块上几乎不变, 候选也必须不变: 每块 PSNR(C,T) ≥ min(该块原生连续帧 PSNR, 50) − 1 dB,
//!    一块都不许低 (HUD 一个字重影就只占一两块, 平均会把它摊没); 再加静止块平均 1−SSIM (同 iq-judge)。
//! 2. **运动块保真**: 其余块上 PSNR(C,T) 要比「直接复制较近的原生帧」高 [`InterpConfig::move_gain_db`],
//!    并且不低于绝对下限 [`InterpConfig::move_floor_db`]。复制帧和前后平均都在这里被判掉。
//! 3. **最差运动块** (运动边缘光晕、遮挡/露出处撕裂): 每个有明显运动的块单独算
//!    MSE(C,T) / MSE(复制较近原生帧, T); 任一块比复制原生帧还差 (比值 > [`InterpConfig::worst_tile_ratio`]) 就不通过。
//! 4. **包络外像素** (沿用 gpuop 的拉扯判据): C 的像素落在 P、N 半径 r 邻域亮度包络外的比例,
//!    跟真值 T 自己落在包络外的比例比 (真值的比例就是原生自噪声), 多出 [`InterpConfig::envelope_margin_pct`] 个百分点就不通过。
//! 5. **细节能量** (同 iq-judge): |log2 E(C)/E(T)| ≤ max(|log2 E(P)/E(T)|, |log2 E(N)/E(T)|) + 0.15。
//!    前后平均、模糊的插帧在这里掉细节。
//!
//! 序列层面 (见 [`judge_sequence`]): 静止块 (HUD) 一帧都不许坏; 其余判不通过的帧不超过 1%, 且不许连续两帧;
//! 插帧整体比原生偏糊/偏亮 (每帧都在容差内, 但隔帧来回变 = 闪烁) 单独判;
//! 快速转镜头的帧单独统计, 不放宽。

use crate::iqjudge::{detail_log2, median, pair_tiles, psnr_of_mse, r4, Shot};
use serde_json::{json, Value};

/// 判据参数。缺省值依据见 `hgamey/_tmp/interp/I2/REPORT.md`。
#[derive(Debug, Clone)]
pub(crate) struct InterpConfig {
    pub tile: usize,
    /// P/T/N 两两块 PSNR 都 ≥ 它, 这块算静止块 (同 iq-judge 的 stable_db)。
    pub static_db: f64,
    /// 静止像素: P/T/N 两两亮度差都不超过它 (灰阶)。
    pub static_px_tol: f32,
    /// 一块里静止像素至少这么多才逐块比 (32×32 块的 1/16)。
    pub min_static_px: usize,
    /// 静止内容逐块: 允许比原生连续帧差多少 dB。
    pub static_margin_db: f64,
    /// 静止块逐块: 原生再干净也只要求到这个 PSNR。
    pub static_cap_db: f64,
    /// 静止块平均 1-SSIM ≤ 系数 × 原生 + 绝对余量。
    pub dssim_factor: f64,
    pub dssim_abs: f64,
    /// 运动块: 至少比复制较近的原生帧高多少 dB。
    pub move_gain_db: f64,
    /// 运动块: PSNR(C,T) 绝对下限。
    pub move_floor_db: f64,
    /// 几乎不动的帧 (复制帧已过下限): 候选要 ≥ min(复制帧, 它)。
    pub still_move_db: f64,
    /// 「有明显运动」的块: 复制较近原生帧在该块的 PSNR 低于它。
    pub visible_motion_db: f64,
    /// 最差运动块 MSE(C,T) / MSE(复制, T) 上限。
    pub worst_tile_ratio: f64,
    /// 包络半径 (像素) 与亮度容差 (灰阶), 同 vkop。
    pub envelope_radius: usize,
    pub envelope_tol: f32,
    /// 候选包络外比例最多比真值多几个百分点。
    pub envelope_margin_pct: f64,
    /// 细节能量 |log2| 余量。
    pub detail_margin: f64,
    /// 序列: 允许判不通过的帧占比 (向下取整)。
    pub max_fail_frac: f64,
    /// 序列: 最多允许连续几帧不通过。
    pub max_consecutive_fail: usize,
    /// 序列: 插帧相对真值的细节 log2 中位数, 绝对值上限 (超了 = 隔帧一糊一清, 闪烁)。
    pub flicker_detail_log2: f64,
    /// 序列: 插帧相对真值的平均亮度差中位数, 绝对值上限 (灰阶)。
    pub flicker_luma: f64,
    /// 序列: 有运动的插帧少于它 → 判不了。
    pub min_motion_frames: usize,
    /// 「快速运动」帧: 复制较近原生帧的全帧 PSNR 低于它。
    pub fast_db: f64,
    /// 序列: 快速运动帧至少要有几帧 (0 = 不要求)。
    pub min_fast_frames: usize,
    pub exclude: Vec<(usize, usize, usize, usize)>,
}

impl Default for InterpConfig {
    fn default() -> Self {
        InterpConfig {
            tile: 32,
            static_db: 45.0,
            static_px_tol: 2.0,
            min_static_px: 64,
            static_margin_db: 1.0,
            static_cap_db: 50.0,
            dssim_factor: 1.5,
            dssim_abs: 0.0005,
            move_gain_db: 10.0,
            move_floor_db: 35.0,
            still_move_db: 40.0,
            visible_motion_db: 35.0,
            worst_tile_ratio: 0.5,
            envelope_radius: 6,
            envelope_tol: 6.0,
            envelope_margin_pct: 0.5,
            detail_margin: 0.15,
            max_fail_frac: 0.01,
            max_consecutive_fail: 1,
            flicker_detail_log2: 0.10,
            flicker_luma: 0.5,
            min_motion_frames: 30,
            fast_db: 25.0,
            min_fast_frames: 0,
            exclude: Vec::new(),
        }
    }
}

/// 一张插帧的判定结果。
#[derive(Debug, Clone)]
pub(crate) struct FrameResult {
    pub ok: bool,
    /// 静止块 (HUD) 不通过: 序列里零容忍。
    pub static_fail: bool,
    /// 有运动块 (否则这帧对插帧好坏没有信息)。
    pub has_motion: bool,
    /// 复制较近原生帧的全帧 PSNR (运动快慢)。
    pub dup_db: f64,
    /// 带符号细节 log2 E(C)/E(T) 与平均亮度差 C−T (序列闪烁用)。
    pub detail_signed: f64,
    pub luma_diff: f64,
    pub fails: Vec<String>,
    pub detail: Value,
}

fn mean(v: &[f32]) -> f64 {
    v.iter().map(|x| *x as f64).sum::<f64>() / v.len().max(1) as f64
}

/// 可分离邻域 min/max (半径 r, 边缘钳位), 两张图取并。与 vkop `envelope` 同义, 只算亮度。
fn envelope(a: &[f32], b: &[f32], w: usize, h: usize, r: usize) -> (Vec<f32>, Vec<f32>) {
    let n = w * h;
    let (mut tlo, mut thi) = (vec![0f32; n], vec![0f32; n]);
    for y in 0..h {
        for x in 0..w {
            let (x0, x1) = (x.saturating_sub(r), (x + r).min(w - 1));
            let (mut lo, mut hi) = (f32::MAX, f32::MIN);
            for xx in x0..=x1 {
                let i = y * w + xx;
                lo = lo.min(a[i]).min(b[i]);
                hi = hi.max(a[i]).max(b[i]);
            }
            tlo[y * w + x] = lo;
            thi[y * w + x] = hi;
        }
    }
    let (mut lo, mut hi) = (vec![0f32; n], vec![0f32; n]);
    for y in 0..h {
        let (y0, y1) = (y.saturating_sub(r), (y + r).min(h - 1));
        for x in 0..w {
            let (mut l, mut u) = (f32::MAX, f32::MIN);
            for yy in y0..=y1 {
                l = l.min(tlo[yy * w + x]);
                u = u.max(thi[yy * w + x]);
            }
            lo[y * w + x] = l;
            hi[y * w + x] = u;
        }
    }
    (lo, hi)
}

fn outside_pct(c: &[f32], lo: &[f32], hi: &[f32], tol: f32, usable_px: &[bool]) -> f64 {
    let (mut bad, mut tot) = (0usize, 0usize);
    for i in 0..c.len() {
        if !usable_px[i] {
            continue;
        }
        tot += 1;
        if c[i] < lo[i] - tol || c[i] > hi[i] + tol {
            bad += 1;
        }
    }
    if tot == 0 {
        0.0
    } else {
        100.0 * bad as f64 / tot as f64
    }
}

/// 判一张插帧。P、T、N 是原生连续三帧, C 是从 P、N 插出来、对应 T 时刻的帧。
pub(crate) fn judge_frame(p: &Shot, t: &Shot, n: &Shot, c: &Shot, cfg: &InterpConfig) -> FrameResult {
    let (w, h, ts) = (t.w, t.h, cfg.tile);
    let (tw, th) = t.tiles();
    let usable: Vec<bool> = (0..tw * th)
        .map(|k| {
            let (x0, y0) = ((k % tw) * ts, (k / tw) * ts);
            !cfg.exclude.iter().any(|(ex0, ey0, ex1, ey1)| x0 < *ex1 && x0 + ts > *ex0 && y0 < *ey1 && y0 + ts > *ey0)
        })
        .collect();
    let pt = pair_tiles(p, t);
    let tn = pair_tiles(t, n);
    let pn = pair_tiles(p, n);
    let ct = pair_tiles(c, t);
    let mse_static = 255.0f64 * 255.0 / 10f64.powf(cfg.static_db / 10.0);
    let mse_vis = 255.0f64 * 255.0 / 10f64.powf(cfg.visible_motion_db / 10.0);
    let k_all = tw * th;
    let is_static: Vec<bool> = (0..k_all)
        .map(|k| usable[k] && pt.mse[k] <= mse_static && tn.mse[k] <= mse_static && pn.mse[k] <= mse_static)
        .collect();
    let is_moving: Vec<bool> = (0..k_all).map(|k| usable[k] && !is_static[k]).collect();
    let n_static = is_static.iter().filter(|x| **x).count();
    let n_moving = is_moving.iter().filter(|x| **x).count();
    let mut fails: Vec<String> = Vec::new();
    let mut static_fail = false;
    let mut d = serde_json::Map::new();
    d.insert("tiles".into(), json!({"static": n_static, "moving": n_moving, "usable": usable.iter().filter(|u| **u).count()}));

    // ① 静止内容逐块。按像素挑: P/T/N 三两之差都 ≤ static_px_tol 灰阶, 且 3×3 邻域都如此 (排掉运动纹理里碰巧相等的点)。
    //    HUD 常和运动的世界落在同一块里, 按整块挑会漏掉它, 所以按像素挑、按块汇总。
    let still_px: Vec<bool> = {
        let raw: Vec<bool> = (0..w * h)
            .map(|i| {
                let (a, b, cc) = (p.y[i], t.y[i], n.y[i]);
                (a - b).abs() <= cfg.static_px_tol && (b - cc).abs() <= cfg.static_px_tol && (a - cc).abs() <= cfg.static_px_tol
            })
            .collect();
        (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                if x == 0 || y == 0 || x + 1 >= w || y + 1 >= h {
                    return false;
                }
                (y - 1..=y + 1).all(|yy| (x - 1..=x + 1).all(|xx| raw[yy * w + xx]))
            })
            .collect()
    };
    let mut n_still_tiles = 0usize;
    {
        let mut se = vec![[0f64; 3]; k_all]; // (C,T) (P,T) (T,N)
        let mut cnt = vec![0usize; k_all];
        for yy in 0..th * ts {
            for xx in 0..tw * ts {
                let i = yy * w + xx;
                let k = (yy / ts) * tw + xx / ts;
                if !still_px[i] || !usable[k] {
                    continue;
                }
                cnt[k] += 1;
                let sq = |a: f32, b: f32| ((a - b) as f64).powi(2);
                se[k][0] += sq(c.y[i], t.y[i]);
                se[k][1] += sq(p.y[i], t.y[i]);
                se[k][2] += sq(t.y[i], n.y[i]);
            }
        }
        let mut bad: Vec<(usize, f64)> = Vec::new();
        let mut worst_margin = f64::INFINITY;
        for k in 0..k_all {
            if cnt[k] < cfg.min_static_px {
                continue;
            }
            n_still_tiles += 1;
            let m = cnt[k] as f64;
            let self_db = psnr_of_mse(se[k][1] / m).min(psnr_of_mse(se[k][2] / m));
            let lim = self_db.min(cfg.static_cap_db) - cfg.static_margin_db;
            let got = psnr_of_mse(se[k][0] / m);
            worst_margin = worst_margin.min(got - lim);
            if got < lim {
                bad.push((k, got));
            }
        }
        let mut sd = json!({"tiles_with_still_px": n_still_tiles, "bad_tiles": bad.len(),
            "worst_margin_db": if worst_margin.is_finite() { json!(r4(worst_margin)) } else { Value::Null }});
        if !bad.is_empty() {
            let (k, got) = bad[0];
            fails.push(format!(
                "静止内容 (HUD/文字/不动背景) 有 {} 块比原生连续帧差, 例如块 ({}, {}) 处 {got:.1} dB: 重影/拖影",
                bad.len(),
                (k % tw) * ts,
                (k / tw) * ts
            ));
            static_fail = true;
        }
        // 整块静止的块再看结构 (1-SSIM, 同 iq-judge)
        if n_static > 0 {
            let avg = |pp: &crate::iqjudge::PairTiles| {
                (0..k_all).filter(|k| is_static[*k]).map(|k| 1.0 - pp.ssim[k]).sum::<f64>() / n_static as f64
            };
            let self_dssim = avg(&pt).max(avg(&tn));
            let c_dssim = avg(&ct);
            let dlim = cfg.dssim_factor * self_dssim + cfg.dssim_abs;
            sd["dssim"] = json!(r4(c_dssim));
            sd["dssim_limit"] = json!(r4(dlim));
            if c_dssim > dlim {
                fails.push(format!("静止块 1-SSIM {c_dssim:.4} > {dlim:.4}"));
                static_fail = true;
            }
        }
        d.insert("static".into(), sd);
    }

    // ②③ 运动块
    let mut dup_mse_frame = 0f64;
    {
        let m = |v: &[f64]| (0..k_all).filter(|k| usable[*k]).map(|k| v[k]).sum::<f64>();
        let nu = usable.iter().filter(|u| **u).count().max(1) as f64;
        dup_mse_frame = dup_mse_frame.max((m(&pt.mse) / nu).min(m(&tn.mse) / nu));
    }
    let dup_db = psnr_of_mse(dup_mse_frame);
    if n_moving > 0 {
        let agg = |v: &[f64]| (0..k_all).filter(|k| is_moving[*k]).map(|k| v[k]).sum::<f64>() / n_moving as f64;
        let c_db = psnr_of_mse(agg(&ct.mse));
        let dup_mov_db = psnr_of_mse(agg(&pt.mse).min(agg(&tn.mse)));
        let gain = c_db - dup_mov_db;
        let mut worst = 0f64;
        let mut worst_k = 0usize;
        let mut n_vis = 0usize;
        for k in (0..k_all).filter(|k| is_moving[*k]) {
            let dm = pt.mse[k].min(tn.mse[k]);
            if dm < mse_vis {
                continue;
            }
            n_vis += 1;
            let r = ct.mse[k] / dm;
            if r > worst {
                worst = r;
                worst_k = k;
            }
        }
        d.insert("moving".into(), json!({"psnr_db": r4(c_db), "dup_psnr_db": r4(dup_mov_db), "gain_db": r4(gain),
            "visible_tiles": n_vis, "worst_tile_ratio": r4(worst),
            "worst_tile_xy": [(worst_k % tw) * ts, (worst_k / tw) * ts]}));
        // 运动看得见 (复制帧低于下限) 时要求比复制帧高 gain; 几乎不动 (复制帧已在下限之上, 实测多是星空闪烁、
        // 编码噪声这类猜不到的变化) 时, 只要求不低于 min(复制帧, still_move_db)。
        let floor_ok = c_db >= cfg.move_floor_db;
        let gain_ok = if dup_mov_db < cfg.move_floor_db {
            gain >= cfg.move_gain_db
        } else {
            c_db >= dup_mov_db.min(cfg.still_move_db)
        };
        if !floor_ok {
            fails.push(format!("运动块 PSNR {c_db:.2} dB < 下限 {:.1} dB", cfg.move_floor_db));
        }
        if !gain_ok {
            fails.push(format!(
                "运动块只比复制原生帧好 {gain:+.2} dB (运动明显时要 ≥ {:.1}; 几乎不动时要 ≥ min(复制帧, {:.0} dB)): 等于没插或插糊了",
                cfg.move_gain_db, cfg.still_move_db
            ));
        }
        if worst > cfg.worst_tile_ratio {
            fails.push(format!(
                "运动块 ({}, {}) 误差是复制原生帧的 {:.2} 倍 (> {:.2}): 运动边缘光晕/遮挡处撕裂",
                (worst_k % tw) * ts,
                (worst_k / tw) * ts,
                worst,
                cfg.worst_tile_ratio
            ));
        }
    }

    // ④ 包络外像素
    {
        let usable_px: Vec<bool> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let (kx, ky) = (x / ts, y / ts);
                kx < tw && ky < th && usable[ky * tw + kx]
            })
            .collect();
        let (lo, hi) = envelope(&p.y, &n.y, w, h, cfg.envelope_radius);
        let c_out = outside_pct(&c.y, &lo, &hi, cfg.envelope_tol, &usable_px);
        let t_out = outside_pct(&t.y, &lo, &hi, cfg.envelope_tol, &usable_px);
        d.insert("envelope".into(), json!({"cand_pct": r4(c_out), "truth_pct": r4(t_out)}));
        if c_out > t_out + cfg.envelope_margin_pct {
            fails.push(format!(
                "{c_out:.2}% 的像素落在前后帧邻域包络外, 真值只有 {t_out:.2}% (容差 {:.1} 个百分点): 凭空造了内容",
                cfg.envelope_margin_pct
            ));
        }
    }

    // ⑤ 细节能量
    let det = detail_log2(c, t, &usable);
    {
        let self_det = detail_log2(p, t, &usable).abs().max(detail_log2(n, t, &usable).abs());
        let lim = self_det + cfg.detail_margin;
        d.insert("detail_log2".into(), json!({"value": r4(det), "limit_abs": r4(lim)}));
        if det.abs() > lim {
            fails.push(format!(
                "细节能量 log2 {det:+.3} 超出 ±{lim:.3} ({})",
                if det < 0.0 { "插糊了/前后平均" } else { "多出高频: 重影双边/噪点" }
            ));
        }
    }

    let luma_diff = mean(&c.y) - mean(&t.y);
    d.insert("dup_db".into(), json!(r4(dup_db)));
    d.insert("luma_diff".into(), json!(r4(luma_diff)));
    FrameResult {
        ok: fails.is_empty(),
        static_fail,
        has_motion: n_moving > 0,
        dup_db,
        detail_signed: det,
        luma_diff,
        fails,
        detail: Value::Object(d),
    }
}

/// 序列判定结果。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SeqVerdict {
    Pass,
    Fail(String),
    Inconclusive(String),
}

impl SeqVerdict {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            SeqVerdict::Pass => "pass",
            SeqVerdict::Fail(_) => "fail",
            SeqVerdict::Inconclusive(_) => "inconclusive",
        }
    }
}

/// 整段判定。`native` = 原生 2 倍帧率连续帧 (至少 3 张), `interp[i]` = 用 native[2i]、native[2i+2] 插出的帧,
/// 对应真值 native[2i+1]。
pub(crate) fn judge_sequence(native: &[Shot], interp: &[Shot], cfg: &InterpConfig) -> (SeqVerdict, Value) {
    let k = interp.len().min(native.len().saturating_sub(1) / 2);
    let mut rep = json!({"n_native": native.len(), "n_interp": interp.len(), "n_judged": k});
    let bad_shape = native.iter().chain(interp).any(|s| s.w != native[0].w || s.h != native[0].h || s.tile != cfg.tile);
    if k == 0 || bad_shape {
        let v = SeqVerdict::Inconclusive("帧数不够 (原生至少 3 张、插帧至少 1 张) 或尺寸不一致".into());
        rep["verdict"] = json!(v.label());
        rep["reason"] = json!(match &v { SeqVerdict::Inconclusive(r) => r.clone(), _ => String::new() });
        return (v, rep);
    }
    let frames: Vec<FrameResult> = (0..k)
        .map(|i| judge_frame(&native[2 * i], &native[2 * i + 1], &native[2 * i + 2], &interp[i], cfg))
        .collect();

    let motion: Vec<usize> = (0..k).filter(|i| frames[*i].has_motion).collect();
    let n_fail = frames.iter().filter(|f| !f.ok).count();
    let n_static_fail = frames.iter().filter(|f| f.static_fail).count();
    let allowed = (cfg.max_fail_frac * k as f64).floor() as usize;
    let mut run = 0usize;
    let mut max_run = 0usize;
    for f in &frames {
        run = if f.ok { 0 } else { run + 1 };
        max_run = max_run.max(run);
    }
    // 按运动快慢分档
    let bucket = |f: &FrameResult| {
        if !f.has_motion {
            "still"
        } else if f.dup_db < cfg.fast_db {
            "fast"
        } else if f.dup_db < cfg.move_floor_db {
            "medium"
        } else {
            "slow"
        }
    };
    let mut buckets = serde_json::Map::new();
    for b in ["still", "slow", "medium", "fast"] {
        let idx: Vec<&FrameResult> = frames.iter().filter(|f| bucket(f) == b).collect();
        buckets.insert(b.into(), json!({"frames": idx.len(), "fail": idx.iter().filter(|f| !f.ok).count()}));
    }
    let n_fast = frames.iter().filter(|f| bucket(f) == "fast").count();
    let det_med = median(motion.iter().map(|i| frames[*i].detail_signed).collect());
    let luma_med = median(frames.iter().map(|f| f.luma_diff).collect());

    let mut fails: Vec<String> = Vec::new();
    if n_static_fail > 0 {
        fails.push(format!("{n_static_fail} 帧静止块 (HUD/文字) 不通过: 静止内容插帧只要照抄, 零容忍"));
    }
    if n_fail > allowed {
        fails.push(format!("{n_fail}/{k} 帧不通过, 超过允许的 {allowed} 帧 ({:.0}%)", cfg.max_fail_frac * 100.0));
    }
    if max_run > cfg.max_consecutive_fail {
        fails.push(format!("连续 {max_run} 帧不通过 (最多允许 {})", cfg.max_consecutive_fail));
    }
    if !motion.is_empty() && det_med.abs() > cfg.flicker_detail_log2 {
        fails.push(format!(
            "插帧整体比原生{} (细节 log2 中位数 {det_med:+.3}, 上限 ±{:.2}): 隔帧一清一糊, 会闪",
            if det_med < 0.0 { "糊" } else { "锐" },
            cfg.flicker_detail_log2
        ));
    }
    if luma_med.abs() > cfg.flicker_luma {
        fails.push(format!(
            "插帧整体比原生{} {:.2} 灰阶 (上限 {:.1}): 隔帧一亮一暗, 会闪",
            if luma_med > 0.0 { "亮" } else { "暗" },
            luma_med.abs(),
            cfg.flicker_luma
        ));
    }
    let mut missing: Vec<String> = Vec::new();
    if motion.len() < cfg.min_motion_frames {
        missing.push(format!("有运动的插帧只有 {} 帧 (< {}), 看不出插帧好坏", motion.len(), cfg.min_motion_frames));
    }
    if n_fast < cfg.min_fast_frames {
        missing.push(format!("快速运动帧只有 {n_fast} 帧 (< {}), 没覆盖快速转镜头", cfg.min_fast_frames));
    }

    let worst: Vec<Value> = {
        let mut idx: Vec<usize> = (0..k).filter(|i| !frames[*i].ok).collect();
        idx.truncate(20);
        idx.iter().map(|i| json!({"interp": i, "truth_native": 2 * i + 1, "bucket": bucket(&frames[*i]),
            "reasons": frames[*i].fails, "detail": frames[*i].detail})).collect()
    };
    rep["frames_fail"] = json!(n_fail);
    rep["frames_fail_allowed"] = json!(allowed);
    rep["frames_static_fail"] = json!(n_static_fail);
    rep["max_consecutive_fail"] = json!(max_run);
    rep["frames_with_motion"] = json!(motion.len());
    rep["buckets"] = Value::Object(buckets);
    rep["flicker"] = json!({"detail_log2_median": r4(det_med), "luma_diff_median": r4(luma_med)});
    rep["failed_frames"] = json!(worst);
    rep["per_frame"] = json!(frames.iter().enumerate().map(|(i, f)| json!({"i": i, "ok": f.ok, "bucket": bucket(f), "d": f.detail})).collect::<Vec<_>>());
    let v = if !fails.is_empty() {
        SeqVerdict::Fail(fails.join("; "))
    } else if !missing.is_empty() {
        SeqVerdict::Inconclusive(missing.join("; "))
    } else {
        SeqVerdict::Pass
    };
    rep["verdict"] = json!(v.label());
    if let SeqVerdict::Fail(r) | SeqVerdict::Inconclusive(r) = &v {
        rep["reason"] = json!(r);
    }
    (v, rep)
}

/// 两张亮度图的平均 = 「前后帧平均」基线。
fn blend(a: &Shot, b: &Shot, tile: usize) -> Shot {
    let y = a.y.iter().zip(&b.y).map(|(p, q)| ((p + q) * 0.5).round()).collect();
    Shot::from_luma(a.w, a.h, y, tile).expect("同尺寸")
}

fn list_frames(spec: &str) -> Result<Vec<String>, String> {
    let p = std::path::Path::new(spec);
    if p.is_dir() {
        let mut v: Vec<String> = std::fs::read_dir(p)
            .map_err(|e| format!("{spec}: {e}"))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file())
            .filter(|p| {
                matches!(p.extension().and_then(|x| x.to_str()).map(|x| x.to_ascii_lowercase()).as_deref(),
                    Some("png" | "jpg" | "jpeg" | "raw"))
            })
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        v.sort();
        Ok(v)
    } else {
        Ok(spec.split(',').filter(|s| !s.is_empty()).map(String::from).collect())
    }
}

/// `phonefarm interp-judge --native <目录|a,b,c…> (--interp <目录|列表> | --interp-full <目录>) [--baselines] …`
/// 退出码 0 = 通过, 1 = 不通过, 3 = 判不了, 2 = 用法错。
pub fn run_interp_judge(args: &[String]) -> i32 {
    let usage = "用法: phonefarm interp-judge --native <目录|a.png,b.png,…> \
(--interp <目录|列表> | --interp-full <目录>) [--baselines] [--exclude x0,y0,x1,y1]… \
[--max-frames N] [--require-fast N] [--per-frame]\n\
  --native       原生按 2 倍帧率的连续帧, 按名字排序; 偶数帧 = 插帧输入, 奇数帧 = 真值\n\
  --interp       只有插出来的帧, 第 i 张对应 native[2i+1]\n\
  --interp-full  插帧器输出的完整 2 倍帧率序列, 取其中奇数帧\n\
  --baselines    另外判「复制前帧」「前后平均」两个基线 (标定用, 两个都应不通过)";
    let mut cfg = InterpConfig::default();
    let (mut native, mut interp, mut full): (Option<String>, Option<String>, Option<String>) = (None, None, None);
    let (mut baselines, mut per_frame, mut max_frames) = (false, false, usize::MAX);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().unwrap_or_default();
        match a.as_str() {
            "--native" => native = Some(val()),
            "--interp" => interp = Some(val()),
            "--interp-full" => full = Some(val()),
            "--baselines" => baselines = true,
            "--per-frame" => per_frame = true,
            "--max-frames" => max_frames = val().parse().unwrap_or(usize::MAX),
            "--require-fast" => cfg.min_fast_frames = val().parse().unwrap_or(0),
            "--min-motion" => cfg.min_motion_frames = val().parse().unwrap_or(cfg.min_motion_frames),
            "--exclude" => {
                let n: Vec<usize> = val().split(',').filter_map(|x| x.trim().parse().ok()).collect();
                if n.len() != 4 || n[0] >= n[2] || n[1] >= n[3] {
                    eprintln!("--exclude 要 x0,y0,x1,y1\n{usage}");
                    return 2;
                }
                cfg.exclude.push((n[0], n[1], n[2], n[3]));
            }
            _ => {
                eprintln!("不认识的参数 {a}\n{usage}");
                return 2;
            }
        }
    }
    let Some(native) = native else {
        eprintln!("{usage}");
        return 2;
    };
    let mut nat_paths = match list_frames(&native) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    nat_paths.truncate(max_frames.saturating_mul(2).saturating_add(1));
    let int_paths: Vec<String> = match (&interp, &full) {
        (Some(s), None) => list_frames(s).unwrap_or_default(),
        (None, Some(s)) => list_frames(s).unwrap_or_default().into_iter().skip(1).step_by(2).collect(),
        (None, None) if baselines => vec![],
        _ => {
            eprintln!("--interp 与 --interp-full 二选一\n{usage}");
            return 2;
        }
    };
    let load_all = |v: &[String]| v.iter().map(|p| crate::iqjudge::load(p, cfg.tile)).collect::<Result<Vec<_>, _>>();
    let nat = match load_all(&nat_paths) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let ints = match load_all(&int_paths) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let strip = |mut r: Value| {
        if !per_frame {
            if let Some(o) = r.as_object_mut() {
                o.remove("per_frame");
            }
        }
        r
    };
    let mut out = json!({"native": native, "native_frames": nat.len()});
    let mut code = 2;
    if !ints.is_empty() {
        let (v, rep) = judge_sequence(&nat, &ints, &cfg);
        code = match v {
            SeqVerdict::Pass => 0,
            SeqVerdict::Fail(_) => 1,
            SeqVerdict::Inconclusive(_) => 3,
        };
        out["candidate"] = strip(rep);
    }
    if baselines && nat.len() >= 3 {
        let k = (nat.len() - 1) / 2;
        let dup: Vec<Shot> = (0..k)
            .map(|i| Shot::from_luma(nat[2 * i].w, nat[2 * i].h, nat[2 * i].y.clone(), cfg.tile).expect("同尺寸"))
            .collect();
        let bl: Vec<Shot> = (0..k).map(|i| blend(&nat[2 * i], &nat[2 * i + 2], cfg.tile)).collect();
        out["baseline_duplicate_prev"] = strip(judge_sequence(&nat, &dup, &cfg).1);
        out["baseline_linear_blend"] = strip(judge_sequence(&nat, &bl, &cfg).1);
        if ints.is_empty() {
            code = 0;
        }
    }
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 256;
    const H: usize = 192;
    const TILE: usize = 32;

    /// vkop `make_reference` 的移植: 斜硬边 + 同心高频环 + 渐变。在比视口宽的「世界」上取景, 镜头横移 = 换取景起点。
    fn world_px(wx: f32, y: usize) -> f32 {
        let (fx, fy) = (wx / W as f32, y as f32 / H as f32);
        let edge = if fx * 0.7 + fy * 0.3 > 0.5 { 1.0 } else { 0.0 };
        let (dx, dy) = (fx - 0.5, fy - 0.5);
        let rings = 0.5 + 0.5 * ((dx * dx + dy * dy).sqrt() * 220.0).sin();
        // 只用亮度: vkop 三通道按 BT.601 合成后的亮度
        let rr = 0.45 * edge + 0.35 * rings + 0.20 * fy;
        let gg = 0.30 * edge + 0.45 * rings + 0.25 * (1.0 - fy);
        let bb = 0.25 * edge + 0.30 * rings + 0.45 * fy;
        let q = |v: f32| (v * 255.0 + 0.5).clamp(0.0, 255.0).floor();
        (0.299 * q(rr) + 0.587 * q(gg) + 0.114 * q(bb)).round()
    }

    /// vkop `hud_band` / `stamp_hud`: 屏幕空间静止的高对比细竖条 (模拟文字)。
    fn hud_band() -> (usize, usize, usize, usize) {
        (W * 6 / 100, H * 72 / 100, W * 34 / 100, H * 92 / 100)
    }
    fn hud_px(x: usize, y: usize) -> Option<f32> {
        let (x0, y0, x1, y1) = hud_band();
        if x >= x0 && x < x1 && y >= y0 && y < y1 {
            Some(if (x / 3) % 2 == 0 && (y / 7) % 3 != 2 { 250.0 } else { 12.0 })
        } else {
            None
        }
    }

    /// 镜头在世界里横移到 `cam` 像素处的一帧 (可加一个跟镜头不同速运动的方块), 盖 HUD。
    fn render(cam: f32, obj_x: Option<f32>) -> Vec<f32> {
        let mut v = vec![0f32; W * H];
        for y in 0..H {
            for x in 0..W {
                let mut px = world_px(x as f32 + cam, y);
                if let Some(ox) = obj_x {
                    if (x as f32) >= ox && (x as f32) < ox + 40.0 && (60..110).contains(&y) {
                        px = 230.0;
                    }
                }
                if let Some(hv) = hud_px(x, y) {
                    px = hv;
                }
                v[y * W + x] = px;
            }
        }
        v
    }

    fn shot(y: Vec<f32>) -> Shot {
        Shot::from_luma(W, H, y, TILE).unwrap()
    }

    /// 原生 2 倍帧率序列: 每帧镜头移 `speed` 像素, 方块每帧移 `obj` 像素。
    fn native_seq(n: usize, speed: f32, obj: Option<f32>) -> Vec<Vec<f32>> {
        (0..n).map(|j| render(j as f32 * speed, obj.map(|o| 30.0 + j as f32 * o))).collect()
    }

    fn cfg() -> InterpConfig {
        InterpConfig { min_motion_frames: 5, ..InterpConfig::default() }
    }

    fn judge(nat: &[Vec<f32>], int: Vec<Vec<f32>>) -> (SeqVerdict, Value) {
        let n: Vec<Shot> = nat.iter().map(|y| shot(y.clone())).collect();
        let i: Vec<Shot> = int.into_iter().map(shot).collect();
        judge_sequence(&n, &i, &cfg())
    }

    fn perfect(nat: &[Vec<f32>]) -> Vec<Vec<f32>> {
        (0..(nat.len() - 1) / 2).map(|i| nat[2 * i + 1].clone()).collect()
    }

    #[test]
    fn perfect_interpolation_passes() {
        let nat = native_seq(21, 3.0, Some(5.0));
        let (v, rep) = judge(&nat, perfect(&nat));
        assert_eq!(v, SeqVerdict::Pass, "{rep}");
        assert_eq!(rep["frames_fail"], json!(0));
    }

    /// 完美插帧加 ±1 灰阶的随机误差 (int8 量化、取整那一级): 仍要通过, 判据不能比原生自噪声还苛刻。
    #[test]
    fn perfect_plus_rounding_noise_passes() {
        let nat = native_seq(21, 3.0, Some(5.0));
        let mut s = 7u64;
        let noisy = perfect(&nat)
            .into_iter()
            .map(|f| {
                f.into_iter()
                    .map(|v| {
                        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                        let r = (s >> 33) % 10;
                        (v + if r == 0 { 1.0 } else if r == 1 { -1.0 } else { 0.0 }).clamp(0.0, 255.0)
                    })
                    .collect()
            })
            .collect();
        let (v, rep) = judge(&nat, noisy);
        assert_eq!(v, SeqVerdict::Pass, "{rep}");
    }

    #[test]
    fn frame_duplication_fails() {
        let nat = native_seq(21, 3.0, Some(5.0));
        let dup = (0..10).map(|i| nat[2 * i].clone()).collect();
        let (v, rep) = judge(&nat, dup);
        match v {
            SeqVerdict::Fail(r) => assert!(r.contains("帧不通过"), "{r}"),
            _ => panic!("复制前帧必须不通过: {rep}"),
        }
        assert!(rep["failed_frames"][0]["reasons"].to_string().contains("等于没插"), "{rep}");
    }

    #[test]
    fn linear_blend_fails_on_motion() {
        let nat = native_seq(21, 3.0, Some(5.0));
        let bl = (0..10)
            .map(|i| nat[2 * i].iter().zip(&nat[2 * i + 2]).map(|(a, b)| ((a + b) / 2.0).round()).collect())
            .collect();
        let (v, rep) = judge(&nat, bl);
        assert!(matches!(v, SeqVerdict::Fail(_)), "{rep}");
        // HUD 不动, 前后平均在 HUD 上是对的: 不能误报成 HUD 问题
        assert_eq!(rep["frames_static_fail"], json!(0), "{rep}");
    }

    /// 没有运动时前后平均等于真值: 判据不能冤枉它, 但也不能拿它当插帧好的证据 (判不了)。
    #[test]
    fn no_motion_is_inconclusive() {
        let nat = native_seq(21, 0.0, None);
        let bl = (0..10).map(|i| nat[2 * i].clone()).collect();
        let (v, rep) = judge(&nat, bl);
        assert!(matches!(v, SeqVerdict::Inconclusive(_)), "{rep}");
        assert_eq!(rep["frames_fail"], json!(0));
    }

    /// 世界插对了, 但 HUD 被当成跟镜头一起动: HUD 区域是「HUD 和移了半步的 HUD」叠在一起 (重影)。
    #[test]
    fn hud_ghosting_fails() {
        let nat = native_seq(21, 3.0, None);
        let (x0, y0, x1, y1) = hud_band();
        let ghost: Vec<Vec<f32>> = perfect(&nat)
            .into_iter()
            .map(|mut f| {
                for y in y0..y1 {
                    for x in x0..x1 {
                        let moved = hud_px(x.saturating_sub(3).max(x0), y).unwrap();
                        f[y * W + x] = ((f[y * W + x] + moved) / 2.0).round();
                    }
                }
                f
            })
            .collect();
        let (v, rep) = judge(&nat, ghost);
        match v {
            SeqVerdict::Fail(r) => assert!(r.contains("静止块"), "{r}"),
            _ => panic!("HUD 重影必须不通过: {rep}"),
        }
        assert!(rep["frames_static_fail"].as_u64().unwrap() == 10, "{rep}");
    }

    /// 方块边缘 3 像素一圈取错 (取前帧): 运动边缘光晕。全帧平均看着还行, 最差块要抓到。
    #[test]
    fn halo_at_moving_edge_fails() {
        let nat = native_seq(21, 1.0, Some(8.0));
        let halo: Vec<Vec<f32>> = (0..10)
            .map(|i| {
                let mut f = nat[2 * i + 1].clone();
                let ox = 30.0 + (2 * i + 1) as f32 * 8.0;
                for y in 56..114usize {
                    for x in 0..W {
                        let xf = x as f32;
                        let near_edge = (xf - ox).abs() < 4.0 || (xf - ox - 40.0).abs() < 4.0 || (56..60).contains(&y) || (110..114).contains(&y);
                        let in_obj_box = xf > ox - 4.0 && xf < ox + 44.0;
                        if near_edge && in_obj_box && hud_px(x, y).is_none() {
                            f[y * W + x] = nat[2 * i][y * W + x];
                        }
                    }
                }
                f
            })
            .collect();
        let (v, rep) = judge(&nat, halo);
        assert!(matches!(v, SeqVerdict::Fail(_)), "{rep}");
        assert!(rep["failed_frames"].to_string().contains("光晕"), "{rep}");
    }

    /// 偶发一帧坏: 1% 以内可以; 连续两帧坏不行。
    #[test]
    fn sporadic_vs_consecutive_failures() {
        let nat = native_seq(401, 3.0, Some(5.0));
        let k = 200;
        let mut one_bad = perfect(&nat);
        one_bad[50] = nat[100].clone();
        one_bad[120] = nat[240].clone();
        let (v, rep) = judge(&nat, one_bad);
        assert_eq!(v, SeqVerdict::Pass, "200 帧坏 2 帧 (1%) 且不相连应通过: {rep}");
        let mut two_bad = perfect(&nat);
        two_bad[50] = nat[100].clone();
        two_bad[51] = nat[102].clone();
        let (v, rep) = judge(&nat, two_bad);
        match v {
            SeqVerdict::Fail(r) => assert!(r.contains("连续 2 帧"), "{r}"),
            _ => panic!("{rep}"),
        }
        assert_eq!(k, (nat.len() - 1) / 2);
    }

    /// 每帧运动区整体亮 1 灰阶: 单帧在运动块容差内, 但隔帧一亮一暗, 序列要判闪烁。
    #[test]
    fn systematic_brightness_is_flicker() {
        let nat = native_seq(21, 3.0, Some(5.0));
        let (x0, y0, x1, _) = hud_band();
        let bright = perfect(&nat)
            .into_iter()
            .map(|f| {
                f.into_iter()
                    .enumerate()
                    .map(|(i, v)| {
                        let (x, y) = (i % W, i / W);
                        let in_hud_tiles = x >= x0 / TILE * TILE && x < (x1 / TILE + 1) * TILE && y >= y0 / TILE * TILE;
                        if in_hud_tiles { v } else { (v + 1.0).min(255.0) }
                    })
                    .collect()
            })
            .collect();
        let (v, rep) = judge(&nat, bright);
        match v {
            SeqVerdict::Fail(r) => assert!(r.contains("会闪"), "{r}"),
            _ => panic!("{rep}"),
        }
    }

    #[test]
    fn envelope_matches_vkop_definition() {
        let a = vec![0f32, 10.0, 20.0, 30.0];
        let b = vec![5f32, 5.0, 5.0, 5.0];
        let (lo, hi) = envelope(&a, &b, 4, 1, 1);
        assert_eq!(lo, vec![0.0, 0.0, 5.0, 5.0]);
        assert_eq!(hi, vec![10.0, 20.0, 30.0, 30.0]);
    }
}
