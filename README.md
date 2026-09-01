# phonefarm

让 AI 自己操作手机的自动化测试框架：一个 Rust 内核驱动设备（Android 模拟器 / OpenHarmony 真机），
用视觉语言模型做决策，自动把 App 的功能页面逛一遍，产出可复盘的完整记录。

核心思路是**弱模型 + 强骨架**：模型只负责'看屏幕、决定下一步'，
其余（采集、校验、防卡死、记账、复盘、性能监控）全由确定性代码兜底。

## 能干什么

- **自动遍历 App**：不登录、弹窗按规则处理、底部标签/频道/设置子页逐个探索，产出页面覆盖清单
- **越跑越熟**：每局结束自动复盘写经验（`lessons.jsonl`），跨局共享交互网（`tree.json`）
- **看得见过程**：每步截图 + 原始 UI 树 + 模型原文 + 系统判定全落盘，`phonefarm show` 逐层回放
- **测性能**：每步采集 68 项遥测（帧率/CPU/内存/温度/电压/GPU/IO/fd…），`phonefarm stats` 出汇总
- **双端适配**：Android（adb）和 OpenHarmony（hdc）同一套逻辑，`--serial hdc:<key>` 一键切换

## 快速开始

```bash
# 1. 配 key
cp secrets.env.example secrets.env   # 填入 GLM_KEY(智谱 Coding 套餐)
# 不填也能启动——程序自动读 ./secrets.env; 仍缺 key 时打印格式说明后退出

# 2. 编译
cd src && cargo build --release && cp target/release/phonefarm ..
# adb 自动定位: ADB_BIN > 仓库根 platform-tools/ > PATH > 常见安装点; 找不到会提示
# OCR 备胎首跑自动编译(需 swiftc); 编不出则该通道自动关闭, 不影响主流程

# 3. 跑一局(Android 模拟器 agentphone 需已启动)
./phonefarm run --task 今日头条遍历 --endless --budget-calls 90 --app com.ss.android.article.news "<目标文本>"

# 4. 跑多轮评测(推荐)
./phonefarm benchmark --task 今日头条遍历 --rounds 10 --budget-calls 90 --app com.ss.android.article.news --json "<目标文本>"

# 5. OpenHarmony 设备(可选)
./phonefarm devices                        # 列出 adb + hdc 两族设备
./phonefarm run --serial hdc:<connect key> --task OH设置冒烟 --budget-calls 30 "<目标>"
```

## 查看结果（CLI 即入口）

所有产物都在盘上，用命令取，不用翻目录：

```bash
phonefarm last                       # 最近一局结论(入口)
phonefarm runs [--task T]            # 某任务全部局
phonefarm show <局ID>                # 局概要: goal/动作/判定/文件清单
phonefarm show <局ID> --step 5       # 第5步: 截图+元素+遥测
phonefarm show <局ID> --raw          # 模型回包原文   --hooks 系统判定
phonefarm show <局ID> --events       # 崩溃/ANR/fd增长事件流
phonefarm show <局ID> --crashes --anr --trace   # 深度调试产物
phonefarm cat <路径>                 # 万能打印: .gz解压/.jsonl美化/.jpg报尺寸
phonefarm stats <局ID>               # 遥测汇总: 帧率/CPU/内存/温度分布
phonefarm schema                     # log.jsonl 全部记录类型说明
phonefarm tasks | tree | lessons | campaign | config
phonefarm probe --serial S "只读命令"  # 设备直连调试; exec 同形但高危需 --yes
```

零背景钻取链：`phonefarm last` → `show <局ID> --step 5` → `cat .../step5.xml.gz`。
局 ID 打前缀即可（多命中会列候选）；所有查看命令只读盘、不烧 token、支持 `--json`。

## 目录结构

```
src/                   Rust 内核(main / runtime / brain / device / tree / fold / cli / telemetry)
phonefarm.toml         配置: 阈值、提示词、provider 链(key 走环境变量)
docs/DESIGN.md         设计文档 v1(架构真理: 记录契约/六步循环/任务隔离)
round.sh               旧版单轮脚本(仍可用; 新流程用 benchmark 直跑)
build_tree.py          离线构建交互网: runs/*/log.jsonl → tree.json(局末自动重算)
summarize_run.py       从 log.jsonl 抽一行汇总(旧脚本, 已被 CLI stats 取代)
ocr.swift / ocr        OCR 文字备胎(macOS Vision, 首跑自动编译)
tasks/<任务>/          各靶子: lessons.jsonl(经验) tree.json(交互网) campaign.tsv(评测账)
                    runs/<局ID>/(log.jsonl + ctx.log + stepN.jpg/.xml.gz, 截图与树不入 git)
```

## 它怎么工作（30 秒版）

每步六拍：**采集**（截图 + UI 树，并行）→ **组装**（上下文 = 目标 + 经验 + 最近几步 + 当前画面）→
**模型决策**（一次最多 4 个动作，后续动作不耗调用）→ **执行前检查**（值域/前科/空白点击，三道确定性门）→
**执行**（换算坐标、等画面安静）→ **验收**（对比差异，判定有无进展）。

细节与演进见 `docs/DESIGN.md`（v1 定稿）与各 SPEC（telemetry/CLI/环境自举，在仓库根或工作区）。

## 相关文档

- `docs/DESIGN.md` — 设计文档 v1（记录契约 / 六步循环 / 任务隔离 / 写入权限）
- 工作区 SPEC：`TELEMETRY_SPEC.md`（遥测数据源）、`CLI_SPEC.md`（查看层命令）、`IMPROVE_SPEC.md`（环境自举）
- `phonefarm schema` — log.jsonl 的完整字段契约（代码内生成，永远最新）
