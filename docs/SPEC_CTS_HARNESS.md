# SPEC: 一致性测试 Harness (CTS / XTS)

版本: v1.1 (2026-09-21) · 实现: `src/cts.rs` · 状态: 已落地
变更: v1.1 补入 OpenHarmony 官方 runner 协议(`aa test` / `OHOS_REPORT_*`)、
`cts-fetch` 结果提取子命令、`test-batch --detach`。v1.0 的全部契约不变。

## 定位

把测试包的批量执行、逐用例结果解析、断言栈提取、收尾对账与标准报告，串成一个
**跨平台的 mini-Tradefed**。两条平台线并列支持，**上层报告格式完全一致**：

| 平台 | 设备通道 | 执行命令 | 输出协议 |
|---|---|---|---|
| Android | `adb` | `am instrument -r -w` | `INSTRUMENTATION_*` |
| OpenHarmony | `hdc` | `aa test`(arkxtest Delegator) | `OHOS_REPORT_*` |

起点是 A2OH 桥接环境：那里没有 cts-tradefed 基础设施(无 adb/fastboot、无完整
system server)，而 CTS 模块本质就是测试 APK + `am instrument`。v1.1 之后本 Harness
不再局限于该场景——**任何 instrumentation 式测试套件**(CTS / XTS / 业务自测包)都能跑，
Android 真机与模拟器同样适用。

与 A2OH 团队《API 桥接的 CTS 快速测试流程》(CTS_FAST_TESTING.md) 的关系:
同事的工具服务单 API 快速闭环(adapter 改动 → 选定切片 → 定位失败,约 50s/轮);
本 Harness 是其上层的**批量挂机执行器**(多模块过夜跑、死锁自愈、证据打包、标准报告)。
两者共享同一套契约,不重复造轮子,不制造标准割裂。

## 架构约束(不可违反)

1. **绝对增量**: run / benchmark / serve 与全部动作原语行为零变化。
   v1.0 新增: `src/cts.rs`、`test-batch` 子命令、script 的 `instrument` 动作、
   Device 的 `stream_shell` / `push_file` / `backend_name`。
   v1.1 新增: `cts-fetch` 子命令、Device 的 `pull_file`、script `instrument` 原语的
   `module` 字段。**Android 既有路径逐行为零变化**——OH 支持全部走新增分支，
   见下文「OH 协议归一化」的零变化承诺。
2. **解耦**: 看门狗、遥测切片、崩溃打包都在 `cts.rs` 外层包装,Device 基础通道
   (`run` / `run_timeout` / `shell`)一行未改。
3. **平台判定不靠猜**: 平台来自显式声明(`--module oh:…` 前缀、profile 的 `platform`
   字段)或设备后端(script 原语跟 `backend_name()` 走)。profile 写了不认识的
   platform 值**直接报错**，不静默回落 Android。

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
 "module": "entry_test",
 "ms": 300000, "idle_ms": 60000,
 "env_args": ["timeout_msec=30000"]}
```

`class_or_method` 缺省 = 整个 runner;Class#method 切片与同事快速闭环同构。
`module` 只在 OH 生效(stage model 的 HAP 模块名),Android 忽略。

**命令合成**(纯函数 `instrument_command`,单测覆盖):

| 平台 | 合成结果 |
|---|---|
| Android | `am instrument -r -w [-e k v]… [-e class Class#method] pkg/runner` |
| OpenHarmony | `aa test -b <bundle> [-m <module>] -s unittest <Runner> [-s class Class#method] [-s k v]…` |

Android 的 runner 已含包名(含 `.`)时原样使用，相对写法(`.FooRunner`)补包名前缀；
OH 的 runner 类名**原样使用不补前缀**(arkxtest Delegator 契约)。
OH 的 timeout 单位是秒，透传 `env_args` 时自行换算——Harness 不替你猜单位。

script 模式下平台跟设备后端走：hdc 设备合成 `aa test`，adb 设备合成 `am instrument`。

### 流式解析状态机(InstrumentParser)

逐行消费运行器输出,每个收官码到达即产出用例结果——**不等整轮结束**。
多行 `stream=`/`stack=` 值按续行拼接,Java 调用栈完整保留。
`Process crashed` 文本置桥崩溃标记,驱动后续恢复流程。

#### OH 协议归一化(入口一层,解析主体零分叉)

OH 官方 runner 的四族输出行与 Android 逐位互为镜像，在 `feed_line` 入口归一化后
交给**同一个**解析主体，因此上层报告、对账、Crash Bundle 全部复用，无第二套实现：

| OH 行 | 归一化为 |
|---|---|
| `OHOS_REPORT_STATUS: k=v` | `INSTRUMENTATION_STATUS: k=v` |
| `OHOS_REPORT_STATUS_CODE: n` | `INSTRUMENTATION_STATUS_CODE: n` |
| `OHOS_REPORT_RESULT: k=v` | `INSTRUMENTATION_RESULT: k=v` |
| `OHOS_REPORT_RESULT_CODE: n` | `INSTRUMENTATION_CODE: n` |
| `OHOS_REPORT_CODE: n` | `INSTRUMENTATION_STATUS_CODE: <oh_case_code(n)>` |

OH 另有一条**独立的用例收官行** `OHOS_REPORT_CODE`，其码值语义与 Android 不同，
由纯函数 `oh_case_code` 映射(单测覆盖全表)：

| OH 码 | 含义 | 映射为 | 判定 |
|---|---|---|---|
| `0` | 通过 | `0` | `PASS` |
| `-1` | 用例错误(未捕获异常) | `-2` | `ASSERTION_FAIL`(栈随 `stack` 字段保留) |
| `-2` | 断言失败 | `-2` | `ASSERTION_FAIL` |
| 其余负值 | 未运行 | `-3` | `NOT_RUN` |
| 非数字 | 无法识别 | `99` | 不匹配任何收官分支，本行忽略 |

两条 OH 专有纪律：

- **用例错误绝不静默零记**：`STATUS_CODE: -1` 在 OH 语境下是"用例抛了未捕获异常"，
  按断言失败记并保留栈。**仅当该行来自 OH 归一化、且当前用例身份已知时**才这样处理；
  用例身份取行内 `class`/`test` 字段，缺失则回落到本用例开始时记下的身份
  (每条收官行处理完都会清空字段表，OH 的收官行往往只剩 `stack`)。
  **Android 既有的 `-1` 路径行为零变化**——Android 行不带 OH 标记，走不进这个分支。
- **双收官去重**：部分 OH 版本对同一用例既发 `OHOS_REPORT_CODE` 又发
  `STATUS_CODE` 收官行，只记一次。Android 每个用例必先 `STATUS_CODE: 1` 重开，
  该标记被清空，不受影响。

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
phonefarm test-batch (--profile P.json
                    | --module pkg/runner
                    | --module oh:bundle/module/Runner
                    | --dir APK目录)
    [--environment E.json] [--serial S] [--include 正则] [--exclude 正则]
    [--resume] [--retry N] [--timeout-ms N] [--idle-timeout-ms N]
    [--heal-script 路径] [--install-cmd 模板{apk}] [--out 目录] [--detach]
```

- **--module**: 两种写法。Android 写 `pkg/runner`；OpenHarmony 写
  `oh:bundle/module/Runner`——三段**都不能省**(stage model 必须给 HAP 模块名)，
  格式不对直接报错并回显收到的原串，不猜。
- **--profile**: 同事的 per-API JSON(`api/module/package/runner/cases[]/
  expected_count/apk_path`),未知字段容忍。cases 逐条展开为 Class#method 切片,
  每条独立 instrument + 独立看门狗 + 独立 Crash Bundle;profile 列出但运行器
  未回报的用例补记 `NOT_RUN`(对账)。
  v1.1 新增两个可选字段：
  - `platform`: `"android"`(缺省) / `"oh"`。写了不认识的值**直接报错**，不回落。
  - `hap_module`(别名 `hap` / `oh_module`): OH 的 HAP 模块名。
    注意与既有的 `module` 字段区分——后者是原始 CTS 模块名，只作报表标识。
- **--environment**: 同事的 environment.json(`device/runtime_version/abi/
  cts_version`),serial 缺省从它取。
- **--dir**: 扫 *.apk → aapt 取包名(无 aapt 用文件名兜底并如实标注)→ 差量部署
  → `pm list instrumentation` 对账 runner;装上了但对不出 runner 的模块如实
  警告待查,不猜。
- **--resume**: 读 `<out>/batch_state.json` 跳过 done 模块。
- **--retry N**: 切片级重试,仅对 ASSERTION_FAIL/TIMEOUT 生效。
- **--detach**: 后台跑，立即回报并退出；进度轮询 `<out>/summary.json`。
  子进程以同参重入(去掉 `--detach` 本身、注入 `--out` 确保父子看同一目录)。
- **--out** 缺省 `cts-batch-<批次ID>`。

## cts-fetch CLI (v1.1)

结果**已经由别的工具落在设备上**时(官方 CTS/XTS 套件、同事的既有流程)，
不重跑，只把结果拉回来扫断言。与 `test-batch` 互补：一个负责跑，一个负责取。

```
phonefarm cts-fetch --remote <设备侧结果路径> [--serial S] [--out 目录]
                    [--pattern 正则] [--max-mb N]
```

- **--remote**(必填): 设备上的结果文件或目录。缺失直接退 2 并提示，不默认猜路径。
- **--serial**: 目标设备；adb 与 hdc 两族都支持。
- **--out**: 本地落地目录，缺省 `cts-fetch-<批次ID>`。
- **--pattern**: 自定义断言行匹配正则；缺省匹配常见断言失败字样。
- **--max-mb**: 单文件大小上限，缺省 16。

纪律：

- **设备侧只读**：只用 `file recv` 拉树(hdc/adb 的 `file recv` 与 `file send` 都是
  顶层命令，不是 `shell` 子命令——v1.1 顺带修正了 `device.rs` 里这处用法)。
  不在设备上写任何文件、不改任何状态。
- **拉不到就报错，不假装成功**：设备无心跳、或设备侧路径不存在/为空(`find` 零产物)时
  直接返回错误，不产出一份"零命中"的报告冒充跑通。
- **跳过要计数**：超过 `--max-mb` 的文件、二进制嗅探命中(首 8KB 含 NUL)、
  媒体与归档扩展名一律跳过，计入 `skipped_files`——"扫了没命中"和"根本没扫"
  在报告里能区分开。
- **上限有标记**：单文件命中超 500 行时截断并置 `truncated`，扫描文件数上限 5000。

产出：

```
<out>/raw/            从设备拉回的原始结果树
<out>/assertions.json 扫描报告
```

`assertions.json` 字段：`scanned_files` / `skipped_files` / `total_matches` /
`files[]`(每项含 `path`、`truncated`、`hits[{line, text}]`)。

若拉回的恰好是本 Harness 自己的批次目录，会额外点出其中的 `summary.json` 与
`junit_*.xml` 位置——那是结构化报告，比断言行扫描更完整，优先看它。

## 验证

**v1.0**

- `cargo test`: 95 passed / 0 failed(新增 9 条: 解析器流式性/跳过不计通过/
  命令组装/日志切片/JUnit 映射/summary 字面值/pm+aapt 解析/profile 兼容/遥测差值)。
- `cargo clippy`: 新代码零告警(存量 26 条历史告警未动)。
- `cargo fmt --check`: `cts.rs` 干净;存量文件的既有偏差保持原样(不对全仓重排,
  增量原则)。
- Windows `cargo check --tests` 通过(顺带修复了 device.rs 一处 unix-only 单测
  在 Windows 下的编译失败: 加 `#[cfg(unix)]`)。

**v1.1**(2026-09-21)

- `cargo test`: **182 passed / 0 failed**。新增 8 条覆盖本次增量:
  - `oh_protocol_parses_pass_fail_stack_and_run_end` — OH 四族行归一化、逐用例流式收官、
    多行栈续行拼接、证据行存**原始** OH 行而非归一化行
  - `oh_case_code_mapping_table` — 收官码映射全表
  - `oh_case_error_neg1_counts_as_fail_never_silent` — OH `-1` 按断言失败记且保栈
  - `oh_android_neg1_unchanged` — **Android 既有 `-1` 路径零变化**(回归护栏)
  - `oh_double_finish_deduped` — 双收官行不双记
  - `oh_command_composition` — `aa test` 与 `am instrument` 两种合成
  - `parse_module_plan_android_and_oh` — `--module` 两种写法与错误格式报错
  - `scan_assertions_finds_failures_skips_binary_and_oversize` — 断言扫描、二进制与超限跳过
- 编译告警数与改动前基线**完全一致**(3 条存量告警，无新增)。
- CLI 冒烟: `cts-fetch` 缺 `--remote` 退 2 并提示；`--module oh:bad` 报格式错误退 2。

**待补**: 真机 E2E。Android 侧需在模拟器跑一轮真实 CTS 模块；OH 侧需在 A2OH 板或
DAYU200 上跑一轮 `aa test`，核对逐用例判定与官方结果一致、失败用例带完整断言栈。
**协议解析目前只有单测夹具证据，尚无真机输出对拍**——按 `GOLDEN_RULES` 第四节第 2 条，
两套协议各验一遍之前，不得宣称双协议已在生产可用。
