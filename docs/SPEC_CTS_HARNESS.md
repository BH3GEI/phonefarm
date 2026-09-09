# SPEC: CTS 自动化测试 Harness (A2OH 桥接环境)

版本: v1.0 (2026-09-08) · 实现: `src/cts.rs` · 状态: 已落地

## 定位

在 OpenHarmony / A2OH 桥接环境上提供轻量化 CTS instrumentation 批量测试能力。
A2OH 侧没有 cts-tradefed 基础设施(无 adb/fastboot、无完整 system server),而 CTS
模块本质就是测试 APK + `am instrument`——本 Harness 把 phonefarm 的 hdc/adb 通道、
遥测与确定性纪律组合成一个 mini-Tradefed。

与 A2OH 团队《API 桥接的 CTS 快速测试流程》(CTS_FAST_TESTING.md) 的关系:
同事的工具服务单 API 快速闭环(adapter 改动 → 选定切片 → 定位失败,约 50s/轮);
本 Harness 是其上层的**批量挂机执行器**(多模块过夜跑、死锁自愈、证据打包、标准报告)。
两者共享同一套契约,不重复造轮子,不制造标准割裂。

## 架构约束(不可违反)

1. **绝对增量**: run / benchmark / script / serve 与全部动作原语行为零变化。
   本特性只新增: `src/cts.rs`、`test-batch` 子命令、script 的 `instrument` 动作、
   Device 的 `stream_shell` / `push_file` / `backend_name` 三个方法。
2. **解耦**: 看门狗、遥测切片、崩溃打包都在 `cts.rs` 外层包装,Device 基础通道
   (`run` / `run_timeout` / `shell`)一行未改。

## 判定枚举(与同事流程逐字对齐)

| 值 | 含义 | 来源 |
|---|---|---|
| `PASS` | 断言全过 | STATUS_CODE 0 |
| `ASSERTION_FAIL` | 断言失败 | STATUS_CODE -2 |
| `ENV_BLOCKED` | 环境阻塞(运行器缺失/掉线/桥崩溃/权限依赖) | spawn 失败、Process crashed、设备失联 |
| `TIMEOUT` | 看门狗强杀(总超时或静默超时) | watchdog |
| `NOT_RUN` | 列出但未产出结果(跳过/忽略/零用例) | STATUS_CODE -3/-4、profile 对账缺测 |

跳过、零用例、运行器错误**不得计为 PASS**。recovery 为 `PENDING` 的批次不得宣称完成。

### 收尾对账(无论一轮以何种方式结束,用例不得凭空消失)

- **在飞用例**: 已开始但从未收官者——看门狗强杀记 `TIMEOUT`;进程崩溃/管道早断记
  `ENV_BLOCKED`(带真实 Class#method,profile 可按名对账)。
- **numtests 缺额**(仅整模块轮次): runner 宣称数 > 实际产出数,缺额补记一条
  `NOT_RUN`(`(runner)#unaccounted_cases_xN`)。切片轮次由 profile 按名对账,不重复记。
- **runner 级错误**(`STATUS Error=…` 或 stderr `INSTRUMENTATION_FAILED`,如包未安装):
  零产出时补记 `ENV_BLOCKED`(`(runner)#runner_error`,附原始报错),不得静默零用例。
- **--resume 续跑**: done 模块跳过且**沿用上一份 summary 的历史报告**计入总账
  (不会把已跑证据抹成全零);failed 模块自动重跑。

## 组件

### instrument 原语(script 动作 & 内部 API)

```json
{"action": "instrument",
 "pkg": "android.content.cts",
 "runner": "androidx.test.runner.AndroidJUnitRunner",
 "class_or_method": "android.content.pm.cts.PackageManagerTest#testGetPackageInfo",
 "ms": 300000, "idle_ms": 60000,
 "env_args": ["timeout_msec=30000"]}
```

组装 `am instrument -r -w [-e k v]… [-e class Class#method] pkg/runner`。
`class_or_method` 缺省 = 整个 runner;Class#method 切片与同事快速闭环同构。

### 流式解析状态机(InstrumentParser)

逐行消费 `am instrument -r` 输出,每个 `INSTRUMENTATION_STATUS_CODE` 到达即产出
用例结果——**不等整轮结束**。多行 `stream=`/`stack=` 值按续行拼接,Java 调用栈
完整保留。`Process crashed` 文本置桥崩溃标记,驱动后续恢复流程。

### 双重看门狗 + 级联清理

- **总超时**(`--timeout-ms`,默认 600s): 单轮 instrument 上限。
- **静默超时**(`--idle-timeout-ms`,默认 90s): 管道无字符流即判锁死(A2OH 管道
  挂起是实测高频故障)。
- 触发后级联: ① `child.kill()` 终止本地管道 → ② 设备端 `am force-stop` +
  `pidof`-kill 强杀被测进程 → ③ HOME 释放输入焦点。已开始未收官的用例如实记
  `TIMEOUT`,流程不中断,继续下一条。

### 掉线自愈(不重用旧 PID 纪律)

桥崩溃/看门狗触发后: 停止设备写操作 → 写 `recovery_pending.json`(batch_id、
boot_id、pending 模块) → 调 `--heal-script` 外部恢复脚本 → 全新会话探活(每次
都是新进程,天然不重用旧 PID) → 核对 `boot_id`(变了=设备重启,旧进程身份作废)
→ 恢复则继续并销记录;未恢复则本模块剩余切片记 `ENV_BLOCKED`,recovery 留
`PENDING`,批次继续下一模块。

### 差量部署(部署耗时的主杠杆)

同事流程实测: Runner 2s,全流程 50s,瓶颈在部署/传输/收证据/恢复。本 Harness:
本地 APK 算 SHA256 ↔ 设备持久缓存 `/data/local/tmp/pf_cts_cache/<name>.sha256`
比对,一致则跳过 push 与 install(每轮只付一次 shell cat)。安装命令对 adb 默认
`pm install -r`;**hdc/A2OH 必须显式给 `--install-cmd`**(桥安装入口由桥定义,
不臆造)。

### Crash Bundle(固定四件,对齐同事证据规范)

失败/超时用例自动打包 `artifacts/<module>/<Class#method>/`:
- `stdout.log` — 用例窗口的运行器原始输出
- `hilog_slice.log` — 设备日志(hdc:`hilog -x`;adb:`logcat -d`)按用例时间窗
  ±5s 切片;时间锚点取设备 `date`,主机时区/时钟偏差不入链;切不出如实标注
- `telemetry.json` — 前后 `vm_rss/fd/socket/threads/pss/crash/anr` 差值
  (FD 差值是 A2OH 死锁排查的关键指标)
- `meta.json` — 判定/耗时/窗口锚点

### 报告

- `junit_<module>.xml`: 标准 JUnit;ASSERTION_FAIL→failure,
  ENV_BLOCKED/TIMEOUT→error, NOT_RUN→skipped(绝不混入通过数)。
- `summary.json`: 批次 totals、五值判定计数、阶段耗时、recovery
  VERIFIED|PENDING、逐模块报告。退出码: 全 VERIFIED 且无
  ASSERTION_FAIL/ENV_BLOCKED/TIMEOUT → 0,否则 1。

## test-batch CLI

```
phonefarm test-batch (--profile P.json | --module pkg/runner | --dir APK目录)
    [--environment E.json] [--serial S] [--include 正则] [--exclude 正则]
    [--resume] [--retry N] [--timeout-ms N] [--idle-timeout-ms N]
    [--heal-script 路径] [--install-cmd 模板{apk}] [--out 目录]
```

- **--profile**: 同事的 per-API JSON(`api/module/package/runner/cases[]/
  expected_count/apk_path`),未知字段容忍。cases 逐条展开为 Class#method 切片,
  每条独立 instrument + 独立看门狗 + 独立 Crash Bundle;profile 列出但运行器
  未回报的用例补记 `NOT_RUN`(对账)。
- **--environment**: 同事的 environment.json(`device/runtime_version/abi/
  cts_version`),serial 缺省从它取。
- **--dir**: 扫 *.apk → aapt 取包名(无 aapt 用文件名兜底并如实标注)→ 差量部署
  → `pm list instrumentation` 对账 runner;装上了但对不出 runner 的模块如实
  警告待查,不猜。
- **--resume**: 读 `<out>/batch_state.json` 跳过 done 模块。
- **--retry N**: 切片级重试,仅对 ASSERTION_FAIL/TIMEOUT 生效。

## 验证

- `cargo test`: 95 passed / 0 failed(新增 9 条: 解析器流式性/跳过不计通过/
  命令组装/日志切片/JUnit 映射/summary 字面值/pm+aapt 解析/profile 兼容/遥测差值)。
- `cargo clippy`: 新代码零告警(存量 26 条历史告警未动)。
- `cargo fmt --check`: `cts.rs` 干净;存量文件的既有偏差保持原样(不对全仓重排,
  增量原则)。
- Windows `cargo check --tests` 通过(顺带修复了 device.rs 一处 unix-only 单测
  在 Windows 下的编译失败: 加 `#[cfg(unix)]`)。
- 真机 E2E(A2OH 板上跑 getPackageInfo profile)需 A2OH 环境,建议合并前由
  桥团队在 DAYU200 上复验一轮。
