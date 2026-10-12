# Megacity 成对采集 —— 「低清+放大」对「原生高清」

只有一台手机, 没法两台同时渲染。办法是利用 Megacity 载体的固定路线: 相机位姿**只由帧序号决定**
(`u = i/(frames-1)`, 不读 deltaTime), 所以同一台手机先后跑两遍, 按**同一个帧序号**取帧,
两遍看到的就是同一个相机位姿, 帧率高低不影响。

## 帧序号从哪来

手机上没有现成办法知道某张截图是第几帧 (两遍帧率不同, 按时间对不上)。所以在灰档层里加了
`framecap` (`knobs/gray/layer/framecap.h`):

- **帧序号** = 载体落 `megacity_started` 标记 (预热结束、进被测窗口) 之后的第几次 present。
  层每次 present 时 stat 一下这个文件, 第一次看见它的那次 present 记 0 号。
- **抓帧** (`debug.knobs.framecap=1`): 选中的帧序号在 present 前把交换链图像拷到主机内存,
  原样写 raw。是游戏交给系统的最终画面 (系统合成器放大到屏幕之前), 无损。
  缺省每 60 帧一张, 外加每 600 帧整数倍处前后各 2 帧 (量对齐误差用)。
- **盖条码** (`debug.knobs.framecap_stamp=1`): 抓完之后在左上角盖一排黑白方块编码帧序号,
  录屏里每一帧都能读出序号, 两遍录屏按序号对齐合成并排视频。抓到的 raw 在盖码之前, 不受影响。
- 每次 present 记一行 `帧序号,CLOCK_MONOTONIC ns` (`present_log.csv`)。

交换链 usage 要加 `TRANSFER_SRC|DST`, 表面不支持就不加并整体停用 (照常出帧, 自报原因)。

## 怎么跑

```bash
# 节点上持锁跑 (pf-node 不等锁, 没轮到会回 "not your turn", 隔一分钟重试即可; 重试不丢队位)
pf-node devlock run pair -- bash loop_v1/carriers/megacity/paired/session.sh \
    --so=knobs/gray/build/out/libVkLayer_refknobs.so --tools=<节点上本目录> \
    <节点上的输出目录> cap:0.5 cap:1.0 perf:0.5 perf:1.0 audit:0.5 cap:1.0:b
```

| 臂 | 做什么 | 进不进性能统计 |
|---|---|---|
| `cap:<resscale>` | 挂层抓帧 + 盖码 + `adb screenrecord` 录屏 | 不进 (有抓帧卡顿和录屏开销) |
| `cap:<resscale>:<后缀>` | 同一档再跑一遍, 量两遍之间的「本底差」(交通等 ECS 系统不完全确定) | 不进 |
| `perf:<resscale>` | 不挂层不录屏, 只读 SurfaceFlinger 上屏时间 + 电池电流电压 | 进 |
| `audit:<resscale>` | frame-audit 拆一帧, 看有没有「低分辨率 → 放大」那一步 | — |
| `upop:<resscale>` | 同 cap, 但层同时开 upop 算子 | 不进 |

收尾 (trap, 被杀也走): 停录屏、杀应用、清 framecap 属性、摘层、`debug.vulkan.layers` 置空、
充电节点若为 0 写回 1、媒体音量若不为 0 置 0, 回读写进 `restore.json`。

会话之后 (不占锁, 纯 CPU, 在节点上就地做, 开发机上行/下行都慢, 大文件别拉):

```bash
python3 framecap_tools.py convert <out>/cap_r0.5/framecap      # raw → png
python3 framecap_tools.py pairs <out>/cap_r0.5 <out>/cap_r1.0 <out>/pairs --floor <out>/cap_r1.0_b
FFMPEG=<ffmpeg> python3 framecap_tools.py video <out>/cap_r0.5 <out>/cap_r1.0 <out>/sbs.mp4 \
    --perf-lr <out>/perf_r0.5/perf.json --perf-hr <out>/perf_r1.0/perf.json
```

节点上没装 ffmpeg, 用 `pip install --target <dir> imageio-ffmpeg` 自带的那个 (不动系统)。

## 口径

- **PSNR**: RGB 三通道 8 bit、全部像素, MSE 取三通道平均, `10·log10(255²/MSE)` dB。
- **MAD**: 同样像素与通道上 `|a-b|` 的平均, 0–255 刻度。
- 两张图都是交换链图像 (Unity 的 Screen 尺寸, 本机 2068×935), 低清遍是 URP `renderScale` 渲染
  再由引擎自己放大到这个尺寸。屏幕本身 2688×1216, 系统合成器那一步放大两遍一样, 不在比较范围内。
- **本底**: 两遍原生高清之间的差。交通/飞艇等 ECS 系统不完全确定, 本底不是无穷大;
  「低清 vs 高清」的差要和它对照看。
- **对齐误差**: 每 600 帧整数倍 k, 低清第 k 帧和高清 k+δ (δ=-2..2) 缩到 1/4 比 MAD, 最小的 δ。

## 自测

```bash
python3 loop_v1/carriers/megacity/paired/test_framecap_tools.py   # 合成数据走一遍 convert/pairs/stamp/video
```
