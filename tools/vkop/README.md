# vkop_runner — Compute Shader 算子的设备侧执行工装

`phonefarm gpu-op` 推到真机上跑的那个二进制。只量数字, 不做判定。

## 出包

```bash
bash tools/vkop/build.sh      # 零交互, 产物 tools/vkop/android_aarch64_vkop_runner
```

依赖 android-commandlinetools 的 NDK (缺省 `28.2.13676358`)，路径可用
`ANDROID_SDK_ROOT` / `VKOP_NDK` 覆盖。与 `../refbench/build/build.sh` 同一套编译模式。

## 单独跑 (调试用; 正式流程走 phonefarm gpu-op)

```bash
adb push tools/vkop/android_aarch64_vkop_runner /data/local/tmp/
adb push candidate.spv /data/local/tmp/
adb shell "cd /data/local/tmp && ./android_aarch64_vkop_runner \
  --shader candidate.spv --track sr --seconds 8 --json"
```

## 口径

- **计时**: `VkQueryPool` 时间戳夹住 `vkCmdDispatch`, 乘 `timestampPeriod`。
  量的是 GPU 执行这一个 dispatch 的时间, 不含 CPU 提交与同步。
- **零拷贝**: 输入/输出都是 `DEVICE_LOCAL` storage image。测量循环里画面全程
  不出显存; 一次上传与一次回读都在计时窗口之外。
- **画质**: 参考图 (高分辨率真值) 盒式下采样成输入, 算子重建回高分辨率,
  PSNR = 重建结果与参考图的峰值信噪比 (RGB 三通道联合 MSE, 像素域 [0,255])。
  缺省参考图是程序化生成的确定性图案 (斜边 / 同心高频环 / 平滑渐变),
  `--reference <rgba8 raw>` 可换成 `phonefarm capture` 抓的真实游戏帧 —— 那才是
  最终该用的真值, 程序化图案只是没有真值时的确定性替代。
- **`--seconds N`**: 按墙上时间持续跑。功耗要在稳定负载下才量得准, 跑两百次
  dispatch 就收工的话, 采样器读到的基本是空闲功耗。

## 2026-09-22 NX809J 首次实测

| 项 | 值 |
|---|---|
| 设备 | Adreno (TM) 840, `timestampPeriod` 52.0833 ns |
| 算子 | `gen1_loc1` (2x2 tap bilinear + clamp), 1280x720 → 1920x1080 |
| GPU 内核时延 | 0.942 ms (中位, 30 次采样, 轮内离散 1.28%) |
| PSNR | 42.826 dB (对程序化参考图) |

回包 schema 见 `docs/SPEC_GPU_OP.md` §6。
