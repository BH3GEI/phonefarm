# 08 copyprobe — 1:1 拷贝探针验收 (2026-09-23)

探针 (`debug.knobs.copyprobe=1`) 把超分要用的机制以最小形态全走一遍：
建 compute pipeline + 建 image + 注入 dispatch + 克隆并替换那一次描述符绑定，
算子是 texelFetch 逐纹素拷贝（游戏放大 shader 的输入逐位相同，画面应逐像素不变）。

## 验收三条，全过

### ① 反作弊放行，能跑满一轮 ✅

5 个探针臂（两批）全部：正常登录 → 点门进大世界 → 跑满 36s 转视角负载，无崩溃、
无反作弊告警。层自报每帧都在干活且从未停用：

| 臂 | frames | copies | subs | disabled |
|---|---|---|---|---|
| 批次1 probe1/2/3 | 4800/4800/5400 | 5392/4791/5096 | 同左 | false |
| 批次2 probe1/probe0 | 5100/5100 | 4793/4792 | 同左 | false |

（copies ≈ frames：每个合成 pass 一次拷贝 + 一次描述符替换）

### ② 画面逐像素不变 ✅（口径见内）

截帧目检：角色、场景、HUD 全部正常，无黑屏无花屏无错位
（`copyprobe_none_134328/{none1,probe0}/shot2.raw` 可对比）。

跨 run 逐像素比对**不可用**：两无层臂控制对全屏差异就有 96.3%
（游戏内时辰不同：none1 白昼 vs probe0 黄昏；待机动画/云/草也在动），
探针臂 vs 无层臂 96.4% 与控制对持平 —— 说明探针没有引入**可检测的额外**差异，
但这条路给不出字面 0 的上限。

逐位不变的依据是构造性的：texelFetch 逐纹素整数搬运（无过滤无浮点舍入），
游戏的放大 shader 拿到逐位相同的输入，同一 GPU 同一 shader 输出必然逐位相同。
若日后要更强证据，可在 shader 里对 src/dst 各算一份校验和自比对。

### ③ 帧时不显著变化 ✅

| 臂 | fps | p50 ms | p95 ms | gpu ms |
|---|---|---|---|---|
| none1 | 59.169 | 16.661 | 19.33 | 13.29 |
| none2 | 59.176 | 16.680 | 19.91 | 13.41 |
| probe（5 臂范围） | 59.11–59.66 | 16.636–16.674 | 19.39–20.16 | 13.17–13.47 |

探针臂 p50 与无层臂差 < 0.05ms，顶满 60 帧档；每帧一次 2140×968 拷贝 + 
一次克隆替换的开销淹没在噪声里。

## 过程中修掉的两个探针 bug

1. **布局判断错误**：第一版拿 pass 49 颜色附件的 `finalLayout`（COLOR_ATTACHMENT_OPTIMAL）
   当注入时点的布局，直接停用。实际上 src 不是合成 pass framebuffer 的附件
   （2140×968 vs 2141×969 挂不上），游戏要在合成 pass 里采样它就必须自己在两 pass
   之间插 transition barrier —— 我们注入的位置排在那道 barrier **之后**，
   src 已是 SHADER_READ_ONLY_OPTIMAL，只做执行依赖即可。
2. **无层臂进世界判定**：亮度方差阈值不可靠（登录页也有 9 万+）；
   且 none 臂没人拉起游戏（有层臂是 mount_layer 拉起的）。改为与门页面截帧做
   像素差 >40% + 显式 `am start`。另踩到一个 shell 坑：管道与 heredoc 抢 stdin
   会让 `python3 -` 读到空程序，静默失败。

## 顺带确认

- 目标绑定是 **binding 0**（composite_draw 溯源 hit）。
- pass_table 的 `frames_seen` 过滤修复生效（主合成 pass begins=4491/frames=4491）。

证据目录：`loop_v1/runs_graylayer/copyprobe_132610/`（3 探针臂）、
`copyprobe_none_134328/`（none1/none2 控制对 + 2 探针臂），
截帧比对 `copyprobe_none_134328/shot_diff.json`。

**结论：机制全链路在真机上每帧走通、反作弊放行、帧时无损。可以投真算子。**
