# 判官体检报告（初版，2026-10-11）

这份报告检验现有判定链（`loopstat::compare` + `sysparam::decide`，画面走 `eval --request`）
在真机噪声下判得准不准。考题答案事先知道：A/A 不该判 KEEP，已知变好应判 KEEP，
已知变差必须 REJECT，画质作弊必须被画面判据拦下。工具是 `phonefarm selftest`，
用法见 [README.md](README.md#selftest判定链的已知答案真机自检)。

**初版状态：只完成了标定。** A/A、已知变好/变差、画质作弊这四类考题一道都还没在真机上跑，
混淆矩阵、误判率、检出率曲线、MDE、噪声底这几张表都还是空的（见 §2）。原因是先后修掉了
两处会让所有测量作废的问题（§4），手机节点又在 16:05 前后失联。下面所有数字都能在
`judge_checkup/` 的证据里查到出处。

## 1. 条件

| 项 | 值 |
|---|---|
| 设备 | 红魔 NX809J（Adreno 840），手机节点，refbench 0.1.0 |
| 负载 | refbench `sr_pipeline` + postfx 插槽装 `selftest/load.spv`（sha256 `c094c3d35efc…`），iters 64，intensity 1 |
| 采集 | 预热 2 s + ftrace 10 s（私有实例，#86），每臂 refbench 固定帧数自退 |
| 风扇 | 开（enable=1 level=5），结束还原 |
| 供电 | 接着 AC，电量 100%。停充**没成**（`status=Charging current_now=0 on_battery=no`），所以功耗不进判定，判定只看帧时 p95 和帧率 |
| GPU | 不锁频。开场 `max_pwrlevel` 被归位到 0，但整段 `thermal_pwrlevel=7`：温控把 GPU 上限压在 578 MHz。重负载臂（≥4000 工作组）全程 578 MHz、忙 100%（trace 头 `gpu_clk=578000000 gpu_busy≈1003k/1003k`） |
| 等冷 | 门槛 46 °C、超时 90 s。实际每臂 2–3 s 就到了（各臂起跑时最热传感器 43.8–45.3 °C） |
| 软件 | 节点 phonefarm main 1db683e（含 #80 #84 #86 #87 #88） |

## 2. 考题进度

| 考题 | 计划 | 状态 |
|---|---|---|
| 标定（帧时 ~ 工作组数） | 0/2000/4000/6000 × 2 轮 | **完成**（§3），4000/6000 档有效 |
| `aa` ×3（误判率） | 每道 10v10 | 未跑 |
| `better:0.5/1/2/5`（检出率 → MDE） | 每道 10v10 | 未跑 |
| `worse:2`（必须 REJECT） | 10v10 | 未跑 |
| `quality`（画质作弊必须 POOR_QUALITY + 对照不得被拦） | 5% 帧时 + 2× 降采样 | 未跑 |
| 原神 A/A（可选） | — | 未跑 |

混淆矩阵 / 误判率 / 检出率 / MDE / 噪声底 / 每多 1 分钟换多少灵敏度：**暂无真机数据**。

## 3. 标定结果（10-11 15:18–15:23，节点任务 bb497a0aed11）

| 工作组 | 第 1 轮 p50 / p95 / fps | 第 2 轮 p50 / p95 / fps | 采集窗 | 有效 |
|---|---|---|---|---|
| 0 | 8.310 / 9.067 / 120.0 | 8.312 / 9.039 / 120.2 | 5.9 / 6.0 s | 否 |
| 2000 | 3.285 / 3.895 / 300.3 | 3.285 / 3.901 / 300.8 | 7.7 / 7.6 s | 否 |
| 4000 | 4.290 / 4.881 / 231.2 | 4.286 / 4.917 / 231.3 | 10.0 / 10.0 s | 是 |
| 6000 | 5.303 / 5.934 / 187.4 | 5.304 / 5.983 / 187.2 | 10.0 / 10.0 s | 是 |

- 拟合（只用有效点）：帧时 p50 = 2.2575 ms + 0.000508 ms × 工作组数，残差 ±0.05%。
  以 4000 工作组为基准，帧时变 1% ≈ 85 个工作组。
- **同档两轮的差**：p50 ≤ 0.1%，p95 0.7–0.8%，fps ≤ 0.1%。只有每档 2 轮，这还不是噪声底，
  只说明这套负载在 578 MHz 温控上限下很稳；真正的噪声底等 A/A 考题。
- 0 和 2000 工作组无效，原因不同：
  - 2000 档 300 fps，3000 帧 10 s 就跑完了，采集窗只剩 7.7 s（判据要求 ≥ 9 s）。正式考题的帧数按标定自动推（最快一臂帧率 × 16 s），不受影响；标定时用 `--frames` 给大一些即可。
  - 0 档帧率读成 120 fps 是解析错：这一档 GPU 几乎空跑，频率在 160 MHz 附近来回调，解析器判成「每帧 3 次提交」（其他档都是 1 次），2145 次提交只算出 714 帧；按 1 次/帧算是约 360 fps，与 3000 帧 8.3 s 跑完对得上。正式考题不用这么轻的档，没修。

证据：[`judge_checkup/cal3_20261011/`](judge_checkup/cal3_20261011)（`calib.json`、每臂 `arm.json` /
`summary.json` / `env.json` / `refbench_out.json` / 过滤后的 trace，约 1.4 MB）。
离线重算任意一臂：`phonefarm parse-trace <trace> --comm RefbenchDrv`。

## 4. 这一轮修掉 / 查清的问题

1. **ftrace 采集被别人常开的事件冲掉（已修，#86 #97）**。手机全局 tracing 上常开 49 个事件
   （sched_* / sde_* 等，系统 perfetto 配置），全局 buffer 每 CPU 8 MB 且扩不上去，一两秒就被冲满：
   10 s 采集只剩约 1 s（大多帧数 0），脚本仍报成功。14:35 那次标定 10 臂没有一臂量成。改成在私有
   ftrace 实例里采之后：8 个 CPU overrun 全 0，帧都在。**用 `ftrace_capture.sh` 的 eval / autoloop
   等在 #86 之前、这台手机当前状态下的测量也受这个影响**；gpu-op 走 Rust 自己的 TraceSession，没查。
2. **原神留在前台发热（已处理，#88）**。之前「待机 46–47 °C 降不到 45 °C」的主要原因是原神在前台
   持续约 60 fps 渲染、refbench 每臂退出后它又回到前台。selftest 开场现在把前台别的应用送回桌面
   （不杀进程）。这次开场前台已经不是原神，等冷 2–3 s 就到 46 °C 以下，所以门槛定 46 °C、超时 90 s。
3. **GPU 频率上限会在会话中途被改（未解决，记录在案）**。开场 `max_pwrlevel=3` 归位到 0，
   收场时又是 3，期间 selftest 没写过它；同一天还见过 17 / 11 / 12。另外 `thermal_pwrlevel=7`
   整段都在。这些都会改变测量条件，长实验里可能造成漂移。ABBA 交替能把慢漂移摊到两臂，
   但要钉死得用 `--lock-freq` 或查清是谁在写。
4. **停充在满电 + AC 时不生效**。功耗因此不进判定（按 eval/autoloop 的 auto 口径），
   本报告的判定只看帧时 p95 和帧率。
5. 节点任务的 `kill` 在 14:40 前后不起作用（返回 `killed: false`），作废的任务只能等它自己跑完。

## 5. 接下来（节点恢复后）

同一份清单、同一组条件、按编号分三段，每段 ≤ 40 分钟（实测每臂约 32 s，一道 10v10 约 11 分钟）：

```
CASES=aa*3,better:0.5,better:1,better:2,better:5,worse:2
COND="--calib judge_checkup/cal3_20261011/calib.json --pairs 10 --cool-c 46 --cool-timeout 90 --lock-label judge"
pf-node -o seg1 phonefarm selftest run --out @out/seg1 --cases $CASES --only aa_1,better1_1,worse2_1 $COND --template <load.spv>
pf-node -o seg2 phonefarm selftest run --out @out/seg2 --cases $CASES --only aa_2,better0p5_1,better2_1 $COND --template <load.spv>
pf-node -o seg3 phonefarm selftest run --out @out/seg3 --cases $CASES --only aa_3,better5_1 $COND --template <load.spv>
pf-node -o q    phonefarm selftest quality --out @out/q --calib … --pct 5 --downs 2 --lock-label judge
# 本地: 三段的考题目录 + 一份 plan.json 合到一个根下
phonefarm selftest report --root merged --ns 5,8,10
```

出来之后本报告补上：混淆矩阵（n = 5/8/10）、A/A 误判率、按幅度的检出率与注入法 MDE、
A/A 噪声（CV）、每个 n 的分钟数，以及画面判据两道题的结论。
