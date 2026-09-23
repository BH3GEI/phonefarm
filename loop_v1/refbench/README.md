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
- **判据 4 的映射**（归因结论 vs 靶子自报瓶颈）冻结在 `rules_frozen.py`
  （生成过现存证据的那份判读源码，原封不动）里，其 sha256 连同 `RULES`（现于
  `src/refbenchreport.rs`）写进报告——事后改规则哈希对不上（harness v2 判据 5
  的种子）。报告本体由 `phonefarm refbench-report --root <runs目录>` 产出。
- **harness v2 衔接**：`run_refbench.sh` 是刻意收窄的负载专属面；负载插件接口
  落地后本文件整体即插件实现，编排一行不用改。

M0 六判据（refbench DESIGN.md §8）：可重复 <0.5% 且单调比 <0.75 · 强度阶梯必须
推动帧时间 · 旋钮两臂 p<0.05 · 三变体归因与自报一致 · 快照零差异 · 离线回放
字节一致。
