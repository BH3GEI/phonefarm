# phonefarm

Agent 自动化移动端并行、测试、采集工具：由 Rust 内核驱动设备（Android 模拟器 / OpenHarmony 真机），通过多模态视觉语言模型进行动作决策，实现移动端应用的深度自动遍历，并提供标准化的运行遥测与状态追溯记录。

程序架构采用“执行回路与决策机制分离”的设计：模型提供屏幕元素识别及下一步动作规划，其余的状态采集、合法性校验、安全拦截、复盘更新和系统性能监控全部由 Rust 宿主进程控制。

## 核心功能

- **应用自动化遍历**：支持无登录态自动探索。基于预置规则拦截系统/应用弹窗，按底部标签页及核心菜单路径深度遍历，输出页面覆盖清单。
- **经验库累积与状态共享**：每轮任务结束自动提炼动作异常或跳转失败场景，追加写入本地 `lessons.jsonl` 经验库；多次运行共享设备状态转移图 `tree.json`。
- **状态持久化与下钻分析**：步骤级截图、原始 UI 树（XML格式）、模型原始 JSON 回包及系统规则判定结果完整落盘，可通过 `phonefarm show` 进行单步状态回溯与调试。
- **多维度性能采集（Telemetry）**：每步执行期间采集 68 项高低频设备指标（含 FPS、Janky 占比、各 CPU 核心频率、内存 Pss 详情、SoC 及电池温度、FD/Socket 占用等），支持通过 `phonefarm stats` 输出局级运行指标汇总。
- **多平台适配**：同时兼容 Android（基于 adb）与 OpenHarmony（基于 hdc）平台，上层执行回路完全复用，支持通过 `--serial` 切换目标设备。

## 快速开始

```bash
# 1. 配置密钥
cp secrets.env.example secrets.env   # 填入 GLM_KEY(智谱 Coding 套餐)
# 密钥不填亦能启动：程序会自动读取 ./secrets.env 补全环境；检测到必选密钥缺失时给出配置说明并安全退出

# 2. 编译构建
cd src && cargo build --release && cp target/release/phonefarm ..
# adb 自动定位：按 ADB_BIN > 仓库根目录 platform-tools/ > PATH 路径 > 常见系统 SDK 目录顺序检索
# OCR 备用文字识别首跑时自动编译（需系统装有 swiftc）；编译失败该辅助通道自动关闭，不影响主回路运行

# 3. 运行 Android 遍历任务（模拟器 agentphone 需提前启动）
./phonefarm run --task 今日头条遍历 --endless --budget-calls 90 --app com.ss.android.article.news "<目标文本>"

# 4. 运行多轮自动化评测
./phonefarm benchmark --task 今日头条遍历 --rounds 10 --budget-calls 90 --app com.ss.android.article.news --json "<目标文本>"

# 5. 运行 OpenHarmony 真机任务
./phonefarm devices                        # 列出当前连接的 adb 与 hdc 设备
./phonefarm run --serial hdc:<serial_id> --task OH设置冒烟 --budget-calls 30 "<目标>"

# 6. 多设备并行（每设备独立一局，stdout 逐行带 [设备] 前缀，任一失败整体退出码非 0）
./phonefarm parallel --job "任务A|目标A|emulator-5554|com.pkg" --job "任务B|目标B|hdc:<key>" --budget-calls 60
# 同一任务名派给多台设备会被拒绝（经验库 lessons/tree 会互踩）——用不同任务名分开，合并语义留后续
```

## 数据查询与 CLI 交互

所有历史对局及性能遥测产物均在本地落盘，通过内置 CLI 命令直观调阅，无需手动检索日志目录：

```bash
phonefarm last                       # 查询最近一局的运行结论及关键指标
phonefarm runs [--task T]            # 列出指定任务下的所有运行局 ID
phonefarm show <局ID>                # 展示该局概要：包含任务目标、执行步骤、判定结果和产物清单
phonefarm show <局ID> --step 5       # 精确查看第 5 步：包含该步截图、UI元素列表、性能遥测快照
phonefarm show <局ID> --raw          # 查看模型的原始回复包内容
phonefarm show <局ID> --hooks        # 打印各步骤对应的内核拦截或规则判定记录
phonefarm show <局ID> --events       # 输出运行期间的异常事件流（崩溃、ANR、FD增长）
phonefarm show <局ID> --crashes      # 输出应用崩溃日志现场
phonefarm show <局ID> --anr           # 输出 ANR 现场记录
phonefarm show <局ID> --trace         # 输出调试跟踪信息
phonefarm cat <文件路径>             # 万能文件查看器：支持 gzip 自动解压、JSONL 格式化、图片属性读取
phonefarm stats <局ID>               # 统计该局运行指标：提供 FPS/CPU/内存/温度的分位数及分布曲线
phonefarm schema                     # 打印 log.jsonl 所有合法记录的字段模型定义（代码内生成，永远最新）
phonefarm tasks | tree | lessons | campaign | config
phonefarm probe --serial S "只读命令"  # 建立与目标设备的只读直连调试通道；exec 命令功能相同但高危需带 --yes
```

分析下钻链路：通过 `phonefarm last` 确认异常局 -> 使用 `show <局ID> --step <N>` 查看特定步骤遥测及上下文 -> `cat .../stepN.xml.gz` 精确提取原始 UI 布局树。局 ID 支持前缀模糊匹配（多项匹配时会输出候选列表），所有查看指令均在本地离线解析，不消耗任何 API 调用额度。

## 目录结构说明

```
src/                   Rust 内核源码（包含运行时、模型调度、设备抽象、CLI、遥测等模块）
phonefarm.toml         主配置文件：包含判定阈值、决策提示词模板、模型 Provider 轮询及降级链
docs/DESIGN.md         设计文档 v1（规定了系统的记录契约、六步执行主循环、任务数据隔离设计）
round.sh               单轮调度外壳脚本（保留以向下兼容，生产环境建议使用 benchmark 命令）
build_tree.py          离线交互网构建器：汇总 runs/*/log.jsonl 导出共享 tree.json
summarize_run.py       旧版数据提取脚本（已被 CLI stats 与 last 命令取代）
ocr.swift / ocr        OCR 辅助识别组件（基于 macOS Vision 库，首跑自动编译）
tasks/<任务名>/        数据隔离目录：包含本地经验库 lessons.jsonl、状态转移图 tree.json、对局汇总 campaign.tsv
                    runs/<局ID>/ （本局的 log.jsonl、ctx.log 详情、步骤截图及 XML UI 树，媒体与树文件不入 git）
```

## 核心执行回路

单步运行遵循以下六个周期：
1. **状态采集**：并行截取当前屏幕并提取 UI 布局树。
2. **上下文组装**：结合任务目标、Lessons 经验、近期历史路径及当前屏幕状态，拼接至提示词上下文。
3. **模型决策**：单次模型调用支持最多 4 组动作规划，非首次动作无需额外模型调用，降低延迟。
4. **执行前校验**：执行滑动距离下限、越权输入检查、高频重复点击保护等三道拦截逻辑。
5. **动作执行**：坐标归一化换算，向目标设备发送输入事件，等待界面定格。
6. **验收差异**：比对动作前后的 UI 树或截图像素差异，进行进展判定、空击仲裁或自愈参数调整。

架构细节与演进规划请参考 `docs/DESIGN.md`（v1 架构定稿）与各功能规格 SPEC。

## 相关文档

- `docs/DESIGN.md` — 设计文档 v1（核心契约 / 状态机循环 / 数据隔离 / 写入规范）
- 工作区 SPEC：`TELEMETRY_SPEC.md`（遥测指标详情）、`CLI_SPEC.md`（命令行交互规范）、`IMPROVE_SPEC.md`（自举部署流程）
- `phonefarm schema` — log.jsonl 全部合法记录的字段模型手册
