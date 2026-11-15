# QUICKSTART：从一台新手机到第一份实测数据（≤30 分钟）

目标：一台手机（Android 或 OpenHarmony）+ 一台常开电脑，半小时内跑通「操作 → 采集 → 下钻」，
需要的话再往前走一步，做一次受控 A/B 实验。

## 0. 准备（5 分钟）

- 手机 USB 插到电脑，开发者选项开 USB 调试。要测功耗、改系统参数的话需要 root（Magisk）；
  只做操作、采集、测试则不需要。
- 测量类任务建议关掉系统自动更新、云同步（它们会在测量中途抢 CPU）。
- 电脑（macOS/Linux）：装好 `adb`（鸿蒙装 `hdc`）与 Rust 工具链，克隆本仓库。
- 编译：`cd src && cargo build --release && cp target/release/phonefarm .. && cd ..`
  （Apple Silicon 再加一句 `codesign --force --sign - ./phonefarm`，原因见 README）。

## 1. 验证设备通路（2 分钟）

```bash
./phonefarm devices                 # 看到你的手机序列号（adb 与 hdc 两族一起列）
./phonefarm preflight               # 环境不变量：风扇/静音/温度（--fix 自动修能修的）
./phonefarm fleet --json | head     # 全场只读快照：电量/温度/风扇/前台应用/设备锁
./phonefarm keepalive               # 唤醒 + 解锁 + 不息屏
```

只做操作和采集时 preflight 不必全过；要做测量就必须全过——风扇开着、媒体没静音、
手机烫手，都会让后面的数字作废。

## 2. 操作设备并采集（5–10 分钟）

挑一条通路：

```bash
# 固定动作脚本（零 Token），每步留截图、UI 树和遥测
./phonefarm script --task 冒烟 --app <包名> --repeat 3 examples/sample_game_benchmark.json

# 或：让 VLM 自主完成一个目标（烧 Token，需要 secrets.env）
./phonefarm run --task 设置冒烟 --budget-calls 30 --app com.android.settings "打开关于手机页面"

# 或：按状态筛选的原始帧采集，给下游训练/评测用
./phonefarm capture --serial <S> --out cap/ --frames 200 --json

# 或：跑一批 CTS/XTS 用例
./phonefarm test-batch --module <pkg>/<runner> --serial <S> --out cts_out/
```

## 3. 下钻（2 分钟）

```bash
./phonefarm last                    # 最近一局的结论与关键指标
./phonefarm show <局ID> --step 3    # 第 3 步的截图、UI 元素、遥测快照
./phonefarm stats <局ID>            # FPS/CPU/内存/温度的分位数
```

全部离线解析本地落盘产物，不消耗 API 额度。所有通路共用同一套遥测，脚本局、VLM 局、CTS 局可以横向比。

## 4. 测量与受控 A/B 实验（可选，需要 root，10 分钟起步）

```bash
# 性能与功耗快照：安卓 sysfs 电源轨 / 鸿蒙 HiSmartPerf，同一份 JSON
./phonefarm perf --serial <S> --app <包名> --json

# 把应用带进一个可重复的测试场景，验证负载可重复，产出 profile.json
#（当前实现面向 3D 实时渲染类应用）
./phonefarm onboard --package <包名> --rounds 3

# 系统参数：先只探测白名单里哪些参数可写、可生效
./phonefarm autoloop --out ev/ --probe-only
# 再跑全自动：冻结规则 → 提名候选 → ABBA 真机评测 → 报告
./phonefarm autoloop --out ev/ --generations 1

# 上层（人/脚本/编排器）提交单个候选的统一入口，契约见 docs/SPEC_GPU_OP.md
./phonefarm eval --request r.json --serial <S> --json
```

判定规则在看到候选数据之前落盘冻结；不显著就回退；退出前恢复所有写入，进出快照逐行相等才算这轮成立。

## 常见坑（都是真事）

- **风扇**：主动散热风扇会吃掉功耗读数，还会被误判成优化收益——preflight 每轮必查。
- **电池满电 / 充电中**：Full 或充电状态下功耗读数是假数，测量必须在放电/旁路态（测量命令自动停充）。
- **温控**：SoC 到温控墙后一切频率读数都失真，等冷是测量的一部分，不是浪费时间。
- **厂商性能服务**：厂商会动态压 CPU/GPU 频率上限，写进去的值可能被抢回；
  `eval` 会回读并把被抢回的那一臂单列，不进统计。
- **设备共享**：碰设备的命令自己拿设备锁、排队等待；别用 `--no-devlock` 绕过去，两个任务同时压一台手机，两边的数都作废。
