# SPEC_PERF_SOURCE —— 性能数据来源抽象（Android sysfs / Android HiSmartPerf / OpenHarmony HiSmartPerf）

> **验证状态**
>
> - **安卓 sysfs 电源轨**：已验证（一直在用）。本轮交叉对比查出并修掉两个功耗口径错，见第 7 节。
> - **安卓 HiSmartPerf**：**2026-09-23 已上真机验证**（红魔 NX809J / Android 16 / 原神 +
>   refbench），与自家 ftrace/sysfs 通路做过同窗口并排对比，见第 7 节。
> - **鸿蒙 HiSmartPerf**：**仍未上真机验证**。本机既没有鸿蒙真机，也没有装 `hdc`。
>   命令口径取自 HiSmartPerf-Editor 的实现与 SmartPerf-Device 官方参数表，解析器由真实采集
>   产物钉死（见「测试夹具的来路」），但**整条设备通路（hdc → SP_daemon → data.csv）
>   一次也没有在真机上跑过**。不要把鸿蒙侧读数当成已验证的实测值。

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

## 6. 鸿蒙侧上真机后要补的验证

> 下面五条**全部只关鸿蒙侧**（`hdc` / `SP_daemon` / `dubai.db`）。本机没有鸿蒙真机，
> 一条都验不了，状态维持「未验证」。安卓侧的验证结果在第 7 节。

1. `hdc -t <k> shell "SP_daemon --version"` 能否拿到版本号（`version_looks_present` 的判据是否够）；
2. `SP_daemon -N 10 -PKG <包名> -c -g -f -t -p` 落盘的 `data.csv` 表头**实际列名**
   是否与本文档一致（表头驱动的解析器能吃下差异，但列名若整体不同则要补映射）；
3. stdout 的逐行布局；
4. 拔掉充电、切 Wi-Fi 后 `currentNow`/`voltageNow` 是否落回可信区间；
5. Xpower `dubai.db` 里比 `SP_daemon -p` 更细的**分器件能耗**（CPU/GPU/display 各吃多少瓦）
   —— 那是 SQLite，本轮没有解析，需要时再定是否引依赖。

## 7. 安卓侧的 HiSmartPerf 通路（2026-09-23 真机验证）

### 7.1 它跟鸿蒙侧不是一条路

安卓侧的 HiSmartPerf **不用 `SP_daemon`**。HiSmartPerf-Editor 5.1.0.47 在安卓上的做法是：

```
adb shell "pidof GamePerfToolCollector"                       # 清残留 → kill -9
adb shell "LD_LIBRARY_PATH=/data/local/tmp/ \
           /data/local/tmp/GamePerfToolCollector -version"    # 比版本, 不一致才重推
adb shell getprop ro.product.cpu.abi                          # 选 plugins/Device/Android/<abi>/
adb push <abi>/{GamePerfToolCollector,libQProfilerInterface.so} /data/local/tmp/
adb shell chmod 777 /data/local/tmp/GamePerfToolCollector
adb shell "LD_LIBRARY_PATH=/data/local/tmp/ \
           /data/local/tmp/GamePerfToolCollector -authorize"  # 常驻, 不返回
adb forward tcp:<本地> tcp:<设备端>                            # 两条: 控制 + 实时数据
```

即：**往手机推一个 native 采集器常驻，再用 `adb forward` 的 socket 实时流拉样本**。
出处逐条在 `app.asar` 的 `dist/electron.js`（推送与启动）与 `dist/umi.js`（socket 与命令）。

设备端采集器读的是 `/sys/class/power_supply/battery/{current_now,voltage_now}`、
`/sys/class/thermal`、kgsl/devfreq，帧率走 `dumpsys SurfaceFlinger --latency`
——**与 `hwcond` 读的是同一批节点**，所以功耗/温度两边本该对得上。

### 7.2 线协议（实测钉死，与照文档推的不一样）

| | 控制通道 | 实时数据通道 |
| :--- | :--- | :--- |
| 设备端口 | 20100..20102 | 20103..20105（**另开一条连接**） |
| 握手 | `0012\|authorize:0;` + `cmd=getVersion;end;` | 同左 |
| 应答分帧 | 按 `;end;` | **无帧尾**，裸 `{…}` |
| 内容 | `value=v1.267;end;` / `ret=0;end;` / `ret=0;info=version:v1.0;gpuType:0;end;` | `{fps:30;refresh:0;…;batTemp:44000;}` |

照 `app.asar` 推出来的第一版有三处是错的，全是上真机才暴露的：

1. **样本只从第二条 socket 出来。** 只连控制通道时 `startCollect` 照样回 `ret=0`，
   然后一条样本都收不到 —— 症状与「目标应用没在前台」一模一样。
2. **握手要读两次。** 第一条应答是裸的 `1.26\0`（协议版本，无 `value=` 无帧尾），
   第二条才是 `value=<采集器版本>;end;`。只读一次就判协议，会把正常握手判成「应答不是这个协议」；
   两条挤在同一次 read 里到达时，`value=` 也不在帧首，只认开头就会把版本号整条漏掉。
3. **两条控制命令不能连发。** 连发会挤进同一个 TCP 段，设备端只解析第一条，
   后一条被整条丢掉**且不报错**：`selectPid` 回 `ret=0`，紧跟着的 `startCollect` 石沉大海，
   数据通道 0 字节。故每条命令都要把回执读干净再发下一条。
   （不传 pid 时只发一条命令反而是对的，所以这个 bug **只在拿得到 pid 时触发**。）

### 7.3 单位（与 sysfs 同刻比对得出）

| 字段 | 采集器 | 同刻 sysfs | 结论 |
| :--- | :--- | :--- | :--- |
| `voltage` | `4207000` | `voltage_now` `4207000` | **逐字透传**，μV |
| `current` | `-87000` | `current_now` `-66000`（充电态在抖） | **逐字透传**同一节点原值 |
| `batTemp` | `44000` | `thermal_zone` `battery` `44000` | **毫摄氏度** |
| `gpuTemp` | `49200..51200` | `thermal_zone` `gpuss-*` `52300..54300` | **毫摄氏度** |

注意 `batTemp` **不是** `/sys/class/power_supply/battery/temp`（那个是 `440`，0.1 °C 制）；
按它折算会把 44 °C 算成 4400 °C。

这台红魔的 `soc` / `cpuTemp` / `shellFrame` / `npuTemp` **恒为 0** —— 它没有采集器要找的
那几个热区名（只有 `gpuss-*` / `skin-msm-therm` / `battery`）。`0` 不是 0 摄氏度，一律 `null`。

### 7.4 同窗口并排对比

`loop_v1/tools/crosscheck_perfsrc.sh` 在**同一个 30 秒窗口**里并排跑两条通路
（先后跑的话，差值里会混进「这两分钟机器变热了」）。
负载 refbench `sr_pipeline`；风扇手动档开启（`fan_state_of_manual=2`）；电池 `Charging`。

| 指标 | HiSmartPerf | 我们的 | 相对差 | 为什么 |
| :--- | ---: | ---: | ---: | :--- |
| 帧率 | 113.867 fps | 122.054 fps（ftrace） | −6.7 % | 口径不同，见下 |
| 帧时均值 | 8.782 ms | 8.191 ms（ftrace） | +7.2 % | 同上（HiSmartPerf 侧是 `1000/每秒fps` 反推） |
| 帧时 p95 | **给不出** | 9.251 ms（ftrace） | — | 实时流没有逐帧间隔 |
| GPU 温度 | 49.347 °C | 50.000 °C（`gpuss-0` 直读） | −1.3 % | 采样时刻不同 |
| 电池温度 | 40.000 °C | 40.000 °C（`thermal_zone battery`） | **0.0 %** | 同一个节点，逐字一致 |
| 整机功耗 | **作废**（电池 `Charging`） | **作废**（读数越界） | — | 见 7.5 |

**帧率那 6.7 % 从哪来**（两条都不算错，是口径差）：

- HiSmartPerf 每秒吐一个**整数** `fps`，窗口首尾那两个**不完整的秒**照样各算一条样本，
  把均值系统性往下拉；
- ftrace 侧数的是 kgsl `adreno_cmdbatch_submitted`，要先自检「每帧几次提交」
  （本轮自检出 **3**，而 refbench 契约里写的是 1 —— 契约那条对这个场景是过时的）。
  两条通路互相印证了这个自检值：若按契约的 1 算，ftrace 会得出 366 fps，与 HiSmartPerf
  差三倍；按自检的 3 算才落在 122 fps，与 HiSmartPerf 的 113.9 差 6.7 %。

**温度对得上**（电池温度逐字一致，GPU 温度差 1.3 % 且方向随采样时刻），
这正是「两边读同一批 sysfs 节点」该有的样子 —— 也说明单位折算没错。

### 7.4b 原神固定场景（2026-09-23，放电态）

同一套并排采法，负载换成原神 7.1.0 定点转视角，**整个窗口停充**（`qcom-battery/charging_enabled`
→ `0`，`status=Discharging`），所以这轮的功耗那一格**两边量的才是同一个东西**。

> 原神 7.1.0 上**手柄注入已失效** —— `loop_v1/scripts/workload_spin_v1.json` 跑出来是静止画面。
> 改用触控版 `knobs/gray/workload_spin_touch_v1.json`，并在窗口内抓两帧算逐像素差把
> 「画面真的在转」钉死：本轮 **10.458 %**（静止画面实测约 1 %）。
> 这条不能省 —— 静止画面照样采得到帧率/功耗/温度，报告看着一切正常。

| 指标 | HiSmartPerf | 我们的 | 相对差 |
| :--- | ---: | ---: | ---: |
| 帧率 | 59.367 fps | 57.739 fps（ftrace） | +2.8 % |
| 帧时均值 | 16.844 ms | 17.313 ms（ftrace） | −2.7 % |
| 帧时 p95 | **给不出** | 21.960 ms（ftrace） | — |
| **整机功耗** | **4.867 W** | **4.979 W** | **−2.3 %** |
| GPU 温度 | 48.070 °C | 48.800 °C | −1.5 % |
| 电池温度 | 38.033 °C | 38.000 °C | +0.1 % |

**功耗对上了。** 放电态下两条通路差 2.3 %，而两边读的是同一对 sysfs 节点、
算瓦数走的是同一份 `watt()` —— 剩下这 2.3 % 只可能来自采样节奏
（HiSmartPerf 每秒一条 vs 我们每 200 ms 一条）。
对照充电态那轮：同一条轨两边分别报 3.086 W 与 777 W，差 250 倍。

**帧率那 2.8 % 的来路，逐秒原始序列直接给出了答案**：

```
41 60 60 60 60 60 60 60 60 60 60 60 61 60 60 60 60 60 60 60 60 60 60 60 60 60 60 59 60 60
```

第一条 `41` 是**不完整的那一秒**（采集从窗口中途接上），照样算一条样本。
去掉首尾两条，HiSmartPerf 的均值是**整整 60.000**。

而 ftrace 侧 `frame_p50 = 16.67 ms` —— **正好就是 60.0 fps**，与 HiSmartPerf 逐字对上。
`fps_mean` 之所以是 57.739，是因为它按**帧时均值** 17.313 ms 反推，而均值被那些
长帧拉高了（`frame_p95 = 21.96 ms`）。

也就是说两条通路在**中位数**上完全一致，差异全部来自**分布的尾巴**：

- 每秒数一次帧的口径，卡顿被摊进那一秒里 —— 一秒内几帧 22 ms 照样凑够 60 帧；
- 逐帧口径能把它们单独拎出来。

这正是 `fps_p95_ms` 在这条通路上必须是 `null` 的实证：
HiSmartPerf 安卓侧**结构上看不见**这些尾巴，拿它的秒级数字算 p95 只会算出一个假的平稳值。
（本轮原神每帧两次提交，`submits_per_frame` 自检出 **2**，与 `loop_v1/README.md` 记录一致。）

### 7.5 交叉对比查出的两个自家口径错

对比表里功耗那一格两边都是「未测到」，但**原因不同**，这个不同就是线索：
HiSmartPerf 算出 3.086 W，我们算出 777 W。同一段窗口、同一块电池，差了 250 倍。

1. **`hwcond::watt()` 原本优先信 `battery/power_now`。**
   本机这个节点读出 `992195296`，按 μW 折算 992 W，而同刻 `voltage_now × current_now`
   只有 0.185 W —— 这个节点在这台机器上给的根本不是 μW。整段窗口 150 条采样**全部**越界。
   HiSmartPerf 的采集器压根不碰 `power_now`，只读 `current_now`/`voltage_now`。
   **口径已改为优先 V × I**，`power_now` 只在电压或电流缺失时兜底。
   `power_now` 是可选节点、各家内核填法不一；而 `voltage_now`(μV)/`current_now`(μA)
   在 power_supply class 里有明确约定。
2. **`power_usable` 原本只查「大于 0」，没有上界。**
   于是 777 W 当成一次正常实测报给上层，`gpu-op` 的功耗门禁拿它做 A/B 裁决 ——
   一个物理上不可能的数字，却因为「大于 0」通过了唯一一道检查。
   已补上**逐条**上界（29 条 5 W 混进一条 300 W，均值 14.8 W 正好落在窗口内，判均值救不了），
   三条通路共用 `hwcond::{POWER_MIN_W, POWER_MAX_W}` 这一把尺子。

另外：**充电态下电池轨读数会落在可信区间内**（实测 0.213 W / 3.086 W），
可信区间拦不住它 —— 它不是整机功耗，只是充电电流与系统耗电相抵之后的余量。
只有电池状态拦得住，故安卓 HiSmartPerf 通路把 `/sys/class/power_supply/battery/status`
一并读进来，非 `Discharging` 一律作废功耗并写明原因。

> **要量真功耗，必须让设备处于放电态。** 本轮设备全程 USB 供电（adb 走 USB），
> 两条通路的功耗都作废了 —— 这是对的行为，不是采集失败。

### 7.6 安卓侧还没验的

1. ~~放电态下两条通路的功耗数值对比~~ —— **已验**，见 7.4b：停充后两边差 2.3 %。
2. ~~原神固定场景下的对比~~ —— **已验**，见 7.4b。
3. **`dumpsys SurfaceFlinger --latency` 在 Android 16 上已失效**（`loop_v1/README.md`
   记过：只回一行 vsync 周期）。但采集器照样给得出 fps，且与 ftrace 对得上 ——
   说明它的帧率另有来路（二进制里同时有 `dumpsys SurfaceFlinger | grep <层名>` 与
   `GetRefresh`/`allSurfaceFlingerCmd` 几条路径），**具体走哪条没有查实**。
4. **`refresh` / `gpuFreq` / `ddrFreq` 恒为 0** —— 未开对应采集位，还是该机型读不到，没有分辨。

## 8. 停充测量（2026-09-23）

### 8.1 为什么必须停充

插着 USB 时，**两条轨量到的都不是整机功耗**：

- **电池轨**：量到的是充电电流与系统耗电**相抵之后的余量**。实测 `0.213 W` / `3.086 W` ——
  稳稳落在可信区间 `0.05..30 W` 里，看着像一个正常的低功耗读数。
- **USB 输入轨**：量到的是墙上功率，**含着灌进电池的那一份**。实测同一时刻
  usb `4.845 V × 1.487 A = 7.20 W`，而 battery `current_now=+771000 μA`（正 = 在充电），
  即 `4.324 V × 0.771 A = 3.33 W` 是在给电池充电 —— **46 % 不是系统在吃的功率**。

两个读数都"合理"，**可信区间一个都拦不住**。而一个看起来合理的错数比一个越界的错数
危险得多：越界的会被拦下，合理的会被下游当成实测功耗拿去做 A/B 裁决。

更要命的是充电电流随电量上升**自己衰减**（CC→CV），是一条单调漂移：
跨时间比较的两个候选之间会凭空多出一个"功耗改善"。
`game_opt_loop` 的 M1 演化 `6.26 W → 5.94 W` 就是这么来的 —— 那 −0.32 W（约 5 %）
只需要 3.3 W 的充电分量衰减 10 % 就能造出来，而报告里**连轨别和电池状态都没记**，
事后完全无法分辨。该结论已撤回。

### 8.2 停充怎么做

`--suspend-charging`（`perf` 与 `gpu-op` 都有，**默认关** —— 这是有副作用的写操作）：
用 root 写 sysfs 让充电停下来，USB 仍连着走 adb，整机改由电池供电，功耗走
**battery 轨 V × I**。

**节点是探出来的，不是写死的。** 不同内核给的不一样，按语义明确程度排序逐个试：

| 节点 | 写什么 |
| :--- | :--- |
| `battery/input_suspend` · `usb/input_suspend` | `1` |
| `battery/charging_enabled` · `battery/battery_charging_enabled` | `0` |
| `battery/charge_control_end_threshold` | 压到**当前电量以下** |
| `usb/input_current_limit`（兜底，动的是输入侧） | `0` |

每个候选都**回读确认停充真的生效**（电流转成放电方向）才算数，没生效就地还原换下一个。

**本机实测结论（2026-09-23，红魔 NX809J / `pmic-glink`）**：

- `power_supply` 下的候选**一个都停不了充**。出厂 `charge_control_end_threshold=80`
  而电量 91 % 照充不误；`usb/input_current_limit=0` 同样无效 —— 写进去不代表内核认。
- 真正管用的是 **`/sys/class/qcom-battery/charging_enabled` 写 `0`**。
  它不在 `power_supply` class 下，而在自己的 class 里，**只看 `power_supply` 会整个错过**。
- 停充后 `status` **立刻**翻成 `Discharging`，但电量计的 `current_now` 要**几秒**才跟上：
  实测三秒内还在报充电方向的 `+1072000 / +982000`，第三秒才翻成 `-325000`。
  所以 `SUSPEND_SETTLE_MS` 定在 12 秒 —— 窗口短于这个延迟，会把一个明明生效了的节点
  判成"写进去无效"再去试下一个（第一版定 2.5 秒就是这么误判的）。

### 8.3 进出快照一致

纪律与锁频那套（`hwcond::Lock`）完全一致：

1. **先落状态文件再动设备** —— 文件里记「改了哪个节点、原值是什么」，进程中途被杀
   也能由下次启动回滚；
2. `perf` / `gpu-op` **每次启动都无条件先 `recover_stale_charge_ctl`**，与本次要不要停充无关
   —— 遗留状态的危害是「手机一直没在充电」，不该等到下次有人恰好又要停充才被清掉；
3. 恢复充电在**打印结果之前**做，且 `gpu-op` 里用宏包住所有中途 `return`
   —— 打印失败、门禁不通过、跑崩，都不能把手机丢在不充电的状态里；
4. 恢复后**回读确认**，不符就留着状态文件并告警，下次启动再试一次。

**电量低于 30 % 不停充**（`MIN_CAPACITY_FOR_SUSPEND_PCT`）：停充期间整机纯靠电池，
跑一轮标尺就是几分钟满载放电，电量本来就低时再抽一把，轻则测到一半关机（这一轮白跑），
重则把电池拖进过放。

**真机验证（2026-09-23）**：`perf --suspend-charging --power-rail battery` 一轮跑完，

| 验的是什么 | 结果 |
| :--- | :--- |
| 哪个节点真能停充 | `/sys/class/qcom-battery/charging_enabled`（`1 → 0`） |
| 测量期间是否真在放电 | `status=Discharging`、`current_now=-358000 μA`、`on_battery=true` |
| 量到的功耗 | `1.572 W`（充电态下同一条轨报的是 `0.213 W` 那种相抵余量） |
| 恢复后进出快照 | 四个候选节点 + `status` **逐字一致**（`charging_enabled` 回到 `1`，`status` 回到 `Charging`） |
| 状态文件 | 已清掉 |

### 8.4 供电状态进报告，且参与判定

- `perf` 的 `meta` 多了 `battery_status` / `battery_current_now_ua` /
  `battery_capacity_pct` / `on_battery`（`power_rail` 本来就有）；
- `gpu-op` 的 `eval_report` 多了 `power_source`（**加性**可选字段，与 `quality_reference`
  同一套路，老消费者解析时忽略即可）：轨别、电池状态、`current_now`、电量、
  `on_battery`、以及这轮停充用的是哪个节点。

**非放电态一律把 `power_watt` 报成 `null` 并写明原因，不进判定。**
判据是 `BatteryState::on_battery()`：`status` 不是 `Charging` **且** `current_now` 确实是负的。
两个判据都要看，因为两个都会单独骗人 —— 停充之后有的内核仍写 `Not charging` 而不是
`Discharging`；而 `current_now` 在充放平衡的瞬间会过零。
