# loop_v1/refbench — 白盒靶子的驱动面

驱动 [HGamey/refbench](https://github.com/HGamey/refbench) 的确定性 Vulkan 负载跑 M0
六条判据。靶子只提供负载/旋钮/ground truth；采集、解析、归因、统计判定全在这边
（复用 `../tools/` 的纯函数链，回放纪律不变）。

```
bash run_m0.sh                    # 全套电池, 零交互, 产物在 ../runs_refbench/
bash run_refbench.sh <label> <outdir> <scene> <intensity> <loadop> <frames>   # 单轮
phonefarm refbench-report --root ../runs_refbench                            # 六判据汇总
```

要点:

- **接口契约**在靶子仓库 `contract/launch.json`：intent 字符串 extras 进，
  `refbench_out.json` 出，进程渲染完固定帧数自退（`pidof` 存活轮询判结束）。
- **trace 过滤**用 `--comm RefbenchDrv`：Adreno Vulkan 驱动在进程内部线程发 kgsl
  提交，靶子把它们改名成稳定契约（细节见靶子 DESIGN.md §2）。每帧恰 1 次提交。
- **无效轮纪律**：采集窗内出现热事件或非干净退出 → 该轮标 `INVALID`，证据保留、
  样本不计、就地重试（至多 2 次）。每轮起跑前有 GPU 温度回落轮询；跑测期间开
  设备风扇、结束还原（风扇不在 38 行快照内，单独存取还原）。
- **判据 4 的映射**（归因结论 vs 靶子自报瓶颈）与全部阈值冻结在 `rules.json`
  （数据，不是代码），报告里的 `rules_sha256` 就是**这份文件的 sha256**——改任何
  一个值，新报告的哈希跟着变（harness v2 判据 5 的种子）。报告本体由
  `phonefarm refbench-report --root <runs目录>` 产出。
- **口径切换点（2026-09-23）**：切换前的报告哈希对应当时的判读源码（原封留在
  `rules_frozen.py`，生成过 `runs_refbench/report.json` 的那份）。因此切换点之前
  的归档在 replay 时会恰好差 `rules_sha256` 一行——记录在案的口径切换，不是回放
  坏了；切换点之后新产的证据哈希互相一致。
- **harness v2 衔接**：`run_refbench.sh` 是刻意收窄的负载专属面；负载插件接口
  落地后本文件整体即插件实现，编排一行不用改。

M0 六判据（refbench DESIGN.md §8）：可重复 <0.5% 且单调比 <0.75 · 强度阶梯必须
推动帧时间 · 旋钮两臂 p<0.05 · 三变体归因与自报一致 · 快照零差异 · 离线回放
字节一致。
