# SPEC_PERF_SOURCE —— 性能数据来源抽象（Android sysfs / OpenHarmony HiSmartPerf）

> **验证状态：鸿蒙侧未上真机验证。** 本机既没有鸿蒙真机，也没有装 `hdc`。命令口径全部取自
> 本机 HiSmartPerf-Editor 1.42 的实现与 SmartPerf-Device 官方参数表，解析器由真实采集
> 产物钉死（见「测试夹具的来路」），但**整条设备通路（hdc → SP_daemon → data.csv）
> 一次也没有在真机上跑过**。上真机前，不要把鸿蒙侧读数当成已验证的实测值。

## 1. 为什么要这层

安卓侧和鸿蒙侧的采集手段没有任何共同点：

| | 安卓 | 鸿蒙 |
| :--- | :--- | :--- |
| 功耗 | `/sys/class/power_supply/{usb,battery}` 电源轨（`src/hwcond.rs`，需 root） | HiSmartPerf：`SP_daemon -p` 的 `currentNow`/`voltageNow`，Xpower 落盘 `dubai.db` |
| 帧时 | raw ftrace 的 kgsl 事件 + Vulkan 时间戳（`src/ftrace.rs` / `src/gpuop.rs`） | `SP_daemon -f` 的 `fpsJitters`（逐帧绘制间隔） |
| 温度 | `/sys/class/thermal/thermal_zone*`（`hwcond` 的冷机门禁） | `SP_daemon -t` 的热区列 |
| 通道 | `adb` | `hdc` |

但上层 `game_opt_loop` 消费的 `contracts/eval_report.schema.json` 只认一组字段：
`operator_latency_ms` / `fps_p95_ms` / `power_watt` / `psnr_db`。

若让上层自己去分辨「这台是安卓还是鸿蒙」，每加一个平台就要改一次上层，
违反金科玉律「**新增一条上层通路不应该修改内核**」。故在 `src/perfsrc.rs` 收口：
**上层只拿 `PerfSnapshot`，不知道底下是哪种设备。**

这层负责其中两个字段 —— `fps_p95_ms` 与 `power_watt`。另外两个不归它：
`operator_latency_ms` 是算子自身的 GPU compute pass 耗时（Vulkan 时间戳，`gpuop`），
`psnr_db` 是画质（`gpuop` 的 quality pass）。

## 2. 契约

```
src/perfsrc.rs     PerfSource trait + PerfSnapshot（统一字段）+ perf 子命令
src/smartperf.rs   鸿蒙侧: SP_daemon / Xpower 的命令构造与解析（全纯函数）
src/hwcond.rs      安卓侧: 电源轨采样与可信度判断（既有代码，未改动）
```

`PerfSnapshot` 的 JSON（两种来源**逐字同构**，由同一个 struct 保证）：

```json
{
  "source": "harmony_smartperf",
  "sample_count": 3,
  "fps": 29.833333333333332,
  "frame_time_mean_ms": 33.300000000000004,
  "fps_p95_ms": 33.3,
  "power_watt": null,
  "soc_temp_c": null,
  "gpu_temp_c": 45.7,
  "battery_temp_c": 40.7,
  "meta": { "parsed_via": "data.csv", "SP_daemon_version": "…" },
  "unavailable": [
    { "field": "power_watt",
      "reason": "3 / 3 条功率读数不在可信区间 0.05..30 W (越界样本如 3052.879 W): 设备多半正插着 USB 充电 …" },
    { "field": "soc_temp_c",
      "reason": "SP_daemon 未上报该热区 (未开 -t, 或该机型没有这个传感器)" }
  ]
}
```

`meta` 是采集侧的旁注，不进判定：鸿蒙侧放设备探测结果与 `parsed_via`（这轮是从
`data.csv` 还是 stdout 解析的）；安卓侧放 `power_rail`（**必须记**：usb 轨量的是墙上
功率、含充电与转换损耗，battery 轨量的是电池真实抽走的功率，两者不可比；不记下来，
下游拿 usb 轨的基线对 battery 轨的候选就会凭空多出一个「功耗改善」）。

`unavailable` 里的字段名 `*` 是特殊值：不是某一个字段缺了，是**整条采集通路不可用**
（设备端没有 `SP_daemon`、不是鸿蒙设备等）。

### 三条纪律

1. **同一份字段** —— 两种来源共用同一个 struct，不存在「安卓多一个字段、鸿蒙少一个字段」。
   单测 `both_sources_emit_exactly_the_same_json_field_set` 逐键比对钉死。
2. **采不到就是 `null`，绝不填 0** —— 一个 `0.000 W` 看起来像个数字，其实是
   「这条轨此刻量不了」，会把上层的 A/B 裁决直接带沟里。每个 `null` 在 `unavailable`
   里配一条人能读的原因。这条纪律对温度同样成立：**`0` 与 `NaN` 都不是测量结果**，
   传感器不支持时设备照样回 `0`（这台机器的 `gpu_max_freq,0` 就是活例），
   把它当成「0 摄氏度」发出去比不给更糟。
3. **不改安卓侧既有行为** —— 安卓实现只是把 `hwcond::read_power` / `power_usable`
   现成的采样与判断包一层，`bench` / `gpu-op` 的调用路径一行未动；两次采样之间的
   200 ms 间隔也与 `gpu-op` 的功耗探针同节拍（不隔开就是「把同一瞬间读 30 遍」，
   报出来却是 `sample_count: 30`，看着像一个 30 秒的窗口）。

## 3. 鸿蒙侧的命令口径（出处逐条可查）

全部取自本机 `/Applications/HiSmartPerf-Editor.app` 的实现与
`/Users/mac/ProgramData/HiSmartPerf_Editor/doc` 的官方文档，**没有一条是凭空构造的**。

| 命令 | 出处 |
| :--- | :--- |
| `hdc -t <k> shell "SP_daemon --version"` | HiSmartPerf-Editor `app.asar` 的设备探测 |
| `hdc -t <k> shell "SP_daemon -deviceinfo"` | 同上 |
| `hdc -t <k> shell SP_daemon -clear` | 同上（采集前清残留） |
| `SP_daemon -N <次数> [-PKG <包名>] -c -g -f -t -p [-r]` | SmartPerf-Device 官方参数表 |
| 结果落盘 `/data/local/tmp/data.csv` | 同上 |
| `hdc -t <k> shell hidumper -s 1213 -a -b` / `-f` / `--dumpDb` | `app.asar` 的 Xpower（dubai）落盘刷写三连 |
| `hdc -t <k> file recv /data/log/xpower/dump/dubai.db <本地>` | 同上 |

官方参数表（`-N` 是唯一必选项）：

| 参数 | 功能 |
| :--- | :--- |
| `-N` | 采集次数（必选） |
| `-PKG` | 包名；不给即整机口径 |
| `-c` / `-g` / `-f` / `-t` / `-p` / `-r` | CPU / GPU / FPS / 温度 / 电流 / 内存 |

### 单位

官方 CSV 字段说明给出的单位：

- `fpsJitters`：单帧绘制间隔，**纳秒**；
- `currentNow`：**mA**；`voltageNow`：**μV**；
- `shell_front` / `shell_frame` / `shell_back` / `soc_thermal` / `system_h`：**摄氏度**。

频率类字段（`cpuFrequ` / `gpuFrequency` / `ddrFrequency`）在文档与实测报告之间
**单位不自洽**（文档写 Hz，而实测报告里的 `cpu-c0-max,3628800` 显然是 kHz）。
故本模块**不做频率换算**，只把原值放进 `SpSample::extras`，**不进统一契约** ——
宁可不给，不给一个单位错的数。

### 解析口径

- **CSV 是默认口径**：`/data/local/tmp/data.csv` 是官方文档指定的产物，列名有据可查。
  解析器**表头驱动**（先读首行拿列名 → 列序号，再按列名派发）—— 设备端的列数随
  `-c/-g/-f/…` 开关和 CPU 核数变化，写死列序号必然在换一台机器时错位。
- **stdout 是退路**：`order:<n>` 开一条新记录，其余 `key=value` 词元归入当前记录。
  这个宽松口径是刻意的 —— HiSmartPerf 自己读 `SP_daemon -N 1 -r` 也是按空白切词、
  认带 `=` 的那个词元，不依赖固定行布局。**stdout 的逐行布局尚未在真机上确认过**，
  只在 CSV 读不到时才用；这轮实际走的是哪条，记在 `meta.parsed_via`。
- **空记录不计数**：重复表头（设备端分段落盘时会再写一次，按内容比对识别，不靠猜首列
  叫什么）、分隔符错位的垃圾行、只有 `order:<n>` 没有数据的记录，全部丢弃。
  收进结果就会虚报 `sample_count`，让一份没有数据的文件看起来采到了东西。

### 功耗的可信区间

`power_watt = |currentNow| / 1000 × |voltageNow| / 1e6`，取绝对值（放电电流在不同内核
里有正有负）。**逐条判，不是判均值**：29 条 5 W 里混进一条 300 W 的充电毛刺，
均值 14.8 W 正好落在 `0.05 W .. 30 W` 窗口内，判均值就会把一个假数字当实测发出去。
只要有一条越界就整组作废 → `null` + 原因 —— 越界意味着这段窗口里供电状态变过，
拿它做 A/B 对比本来就不成立。

这条门槛不是拍脑袋：2026-09-18 本机实测时手机插着 USB 充电，HiSmartPerf 采到的是
**电流 −724805 mA、功率 −3053229 mW**（折合三千瓦）。工具自己也弹了
「USB 连接且正在充电会造成功耗测试不准确」。要量鸿蒙侧功耗，必须切 Wi-Fi 连接、
拔掉充电 —— 与安卓侧 `hwcond::power_usable` 的判断同源同理。

## 4. 用法

```bash
# 采一次归一化快照（零 Token，不改设备任何配置）。后端由 --serial 决定：
#   hdc:<connect key> → 鸿蒙 HiSmartPerf；其余 → 安卓 sysfs 电源轨
./phonefarm perf --serial hdc:<connect key> --app com.example.demo --rounds 30 --json
./phonefarm perf --serial emulator-5554 --power-rail usb --json

# 额外刷 Xpower 落盘并把 dubai.db 拉回本机（有副作用，默认不做，见下）
./phonefarm perf --serial hdc:<connect key> --xpower-out ./xpower --json

# 离线口径：解析一份已经拉回本机的 data.csv，完全不碰设备
# （同目录若有 HiSmartPerf 报告的 general.csv，一并读进 meta 当本轮采集元数据）
./phonefarm perf --from-csv /path/to/data.csv --json
```

退出码：`0` = 采到了至少一个采样点；`1` = 设备在但什么都没采到；`2` = 参数错、
设备不可用，或整条采集通路不可用（`unavailable` 里出现字段名 `*`）。

鸿蒙侧采集会在设备上写两处：`SP_daemon` 自己落盘的 `/data/local/tmp/data.csv`，
以及采集前对它的一次 `rm -f`。**这次 `rm` 是必须的** —— 上一轮的 `data.csv` 会原地留着，
不删掉，这一轮若超时或失败，`cat` 读回来的就是上一轮的数据，一份陈旧读数冒充本次实测，
比采不到更糟。

`--xpower-out` 是**唯一**会动 Xpower 的开关，默认关：刷盘要先 `rm -rf` 设备上的旧
`dubai.db` 再跑三条 `hidumper`，是有副作用的写操作；而本模块目前不解析 SQLite，
拉回来只是留给人或别的工具接手。不主动删设备上的东西。

## 5. 测试夹具的来路

`src/testdata/sp_daemon_data.csv` 与 `src/testdata/hismartperf_general.csv` 来自
**2026-09-18 本机用 HiSmartPerf 实测原神 119 秒**的真实产物，已脱敏（去掉机型、
包名、序列号与机型相关的 CPU 簇频率行），各只留下够钉住行为的最小行数：

- 帧间隔 **33.3 ms**（锁 30 帧，两分钟几乎没有一帧偏离）→ 钉住 `fpsJitters` 纳秒换算与 p95；
- `gpu_max_freq,0` / `gpu_min_freq,0`（该机型这套组合根本采不到 GPU 频率）→ 钉住「0 不是一个测量值」；
- 电流 −724805 mA / 电压 4.212 V（充电态垃圾值）→ 钉住功耗可信区间必须把它挡在契约外；
- `soc_thermal` / `shell_frame` 两列为空 → 钉住缺失热区必须是 `null` 而不是 0。

`SP_daemon` 的 **stdout 布局**与 `-deviceinfo` 的字段名没有真机样本，
对应夹具在单测里以内联字符串出现并显式标注「构造样本，非真机抓取」，
只用来钉住解析行为本身，不冒充实测。

## 6. 上真机后要补的验证

1. `hdc -t <k> shell "SP_daemon --version"` 能否拿到版本号（`version_looks_present` 的判据是否够）；
2. `SP_daemon -N 10 -PKG <包名> -c -g -f -t -p` 落盘的 `data.csv` 表头**实际列名**
   是否与本文档一致（表头驱动的解析器能吃下差异，但列名若整体不同则要补映射）；
3. stdout 的逐行布局；
4. 拔掉充电、切 Wi-Fi 后 `currentNow`/`voltageNow` 是否落回可信区间；
5. Xpower `dubai.db` 里比 `SP_daemon -p` 更细的**分器件能耗**（CPU/GPU/display 各吃多少瓦）
   —— 那是 SQLite，本轮没有解析，需要时再定是否引依赖。
