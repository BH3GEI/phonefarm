#!/usr/bin/env python3
"""spin_check.py — 「视角到底转没转」的判据 (纯函数)。

为什么需要这个
--------------
原神 7.1.0 之后手柄注入不再被游戏接受。失效是**安静的**: `phonefarm script`
照常跑完、退出码正常、ftrace 照样有帧时序 —— 只是画面根本没动, 采到的是静止场景。
这种数据看起来完全正常, 混进 A/B 比较里没有任何一处会报错。

所以负载不能只看"脚本跑完了", 要看**画面真的在变**。做法是在拖拽进行中抓两帧
原始帧缓冲, 数一数有多少像素真的变了:

- 转视角: 整个画面在平移, 绝大多数像素都会变 (实测远超门槛)
- 静止场景: 只有草、云、UI 时钟这类小动画在动, 变的像素是个位数百分比

门槛定在 20%: 离两边都远, 不需要精调。

`screencap` 原始格式
--------------------
`adb exec-out screencap` (不带 `-p`) 给的是「小端 32 位宽、高、格式 [, 色彩空间]」
的头 + RGBA 像素。头长度各版本不一样 (12 或 16 字节), 所以不写死 ——
用 `len(data) - w*h*4` 反推, 对不上就如实报错, 不瞎猜。
"""
from __future__ import annotations
import struct

# 单通道差值超过这个数才算「这个像素变了」—— 滤掉编码噪声与极轻微的明暗浮动
CHANNEL_DELTA = 16
# 变化像素占比超过这个数才算「视角在转」
MOVED_FRAC_GATE = 0.20
# 采样步长 (每 N 个像素看一个)。全量比对 13MB x2 在纯 Python 里太慢, 而平移是
# 全画面性质的, 均匀抽样完全够用。
SAMPLE_STRIDE = 64
# 抽样点数的下限。帧太小时自动把步长压下来, 免得退化成只看几个像素。
MIN_SAMPLES = 1024


def parse_screencap(data: bytes) -> tuple[int, int, bytes]:
    """原始 screencap → (宽, 高, RGBA 像素)。头长度反推, 不写死。"""
    if len(data) < 16:
        raise ValueError(f"screencap 数据太短 ({len(data)} 字节)")
    w, h = struct.unpack_from("<II", data, 0)
    if not (0 < w <= 20000 and 0 < h <= 20000):
        raise ValueError(f"screencap 头不合理: w={w} h={h}")
    want = w * h * 4
    head = len(data) - want
    if head not in (12, 16):
        raise ValueError(
            f"screencap 头长度算出来是 {head} 字节 (只认 12/16): "
            f"w={w} h={h} 数据 {len(data)} 字节")
    return w, h, data[head:]


def moved_fraction(a: bytes, b: bytes) -> tuple[float, int]:
    """两帧之间「变了的像素」占抽样点的比例。返回 (比例, 抽样点数)。"""
    wa, ha, pa = parse_screencap(a)
    wb, hb, pb = parse_screencap(b)
    if (wa, ha) != (wb, hb):
        raise ValueError(f"两帧尺寸不同: {wa}x{ha} vs {wb}x{hb}")
    n = min(len(pa), len(pb)) // 4
    if n == 0:
        raise ValueError("帧里一个像素都没有")
    # 步长要保证抽到足够多的点。固定步长 64 在 1216x2688 上有 5 万个样本, 绰绰有余;
    # 但帧一小 (测试用的小图, 或某些设备的缩略帧) 就会退化成只抽到个位数个点,
    # 那时一个像素变了就等于 100% 变了。所以按帧大小夹一下, 至少抽 1024 个点。
    stride = max(1, min(SAMPLE_STRIDE, n // MIN_SAMPLES))
    moved = 0
    total = 0
    for i in range(0, n, stride):
        o = i * 4
        total += 1
        # 只看 RGB, 不看 A —— 不透明画面的 A 恒为 255, 带不进任何信息
        if (abs(pa[o] - pb[o]) > CHANNEL_DELTA
                or abs(pa[o + 1] - pb[o + 1]) > CHANNEL_DELTA
                or abs(pa[o + 2] - pb[o + 2]) > CHANNEL_DELTA):
            moved += 1
    return (moved / total if total else 0.0), total


def verdict(a: bytes, b: bytes) -> dict:
    """两帧 → 「视角是否在转」的结论。"""
    try:
        frac, total = moved_fraction(a, b)
    except ValueError as e:
        return {"spinning": False, "error": str(e), "gate": MOVED_FRAC_GATE}
    return {
        "spinning": frac >= MOVED_FRAC_GATE,
        "moved_fraction": round(frac, 4),
        "gate": MOVED_FRAC_GATE,
        "sampled_pixels": total,
        "note": ("画面在动, 负载生效" if frac >= MOVED_FRAC_GATE else
                 "两帧几乎一样 —— 负载没有真的转视角。原神 7.1.0 之后手柄注入"
                 "已失效且失效是安静的, 请确认用的是触控版负载, 且游戏在大世界探索态"),
    }
