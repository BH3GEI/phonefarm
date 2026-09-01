# phonefarm

Agent 自动化移动端并行、测试、采集工具：由 Rust 内核驱动设备（Android 模拟器 / OpenHarmony 真机），通过多模态视觉语言模型进行动作决策，实现移动端应用的深度自动遍历，并提供标准化的运行遥测与状态追溯记录。

宿主程序架构实现了执行回路与决策机制的分离。模型提供屏幕多模态动作决策，状态采集、规则拦截、复盘和系统监控全部由 Rust 宿主进程控制。

## 常用命令

```bash
# 编译构建
cd src && cargo build --release && cp target/release/phonefarm ..

# 运行单次遍历任务（Android）
./phonefarm run --task 今日头条遍历 --endless --budget-calls 90 --app com.ss.android.article.news "<目标文本>"

# 运行多轮评测与指标统计
./phonefarm benchmark --task 今日头条遍历 --rounds 10 --budget-calls 90 --app com.ss.android.article.news --json "<目标文本>"

# 运行 OpenHarmony 设备任务
./phonefarm run --serial hdc:<serial_id> --task OH设置冒烟 --budget-calls 30 "<目标>"

# 使用 CLI 解析运行记录（不消耗 API 调用额度）
./phonefarm last                                  # 最近一局运行结论与性能概要
./phonefarm runs [--task T]                       # 指定任务的所有运行历史
./phonefarm show <局ID> [--step N]                # 局级概要 / 步骤级上下文与遥测快照
./phonefarm show <局ID> --raw|--hooks|--events    # 模型回复原文 / 拦截规则记录 / 异常事件流
./phonefarm stats <局ID>                          # 局级多维度性能遥测统计指标
./phonefarm cat <文件路径>                         # 压缩文件、JSONL 美化及图片万能查看器
./phonefarm schema                                # 输出运行日志 log.jsonl 的完备模型 schema
./phonefarm probe --serial S "只读命令"             # 目标设备只读调试通道
```

## 安全边界与执行规范

- **财务约束**：多模态大模型调用会产生真实的 API Token 消耗。在执行涉及真机测试、大规模并行对局或高并发压力测试前，必须向用户进行执行预算汇报，经明确授权同意后方可运行；单测运行、CLI 数据解析、本地离线编译可自由执行。
- **密钥管理**：程序在运行时自动检测并加载 `./secrets.env`。缺失密钥时，程序会提示格式并退出。严禁在代码、日志及任何可提交文件（如 `AGENTS.md`、`README.md` 等）中硬编码真实 API 密钥。
- **测试环境约束**：Android 设备统一使用 AVD 模拟器（AVD 名称：`agentphone`）；OpenHarmony 测试需在特定的 intel-mac 物理中转端上通过 ssh 控制，连接真机前需核实目标物理端是否完成本地构建与代码同步。
- **数据持久化规范**：程序唯一合法的写入路径为 `tasks/<任务名>/` 目录。对运行日志 `log.jsonl` 仅执行追加（append）写入；对 `lessons.jsonl` 的更新必须保证原子写（atomic write）；禁止自行清理 runs/ 历史目录，相关的媒体或树结构大文件已由 `.gitignore` 规则排除，严禁人为提交大体积非文本文件至 Git 仓库。
- **代码稳定性**：任何逻辑或代码变更后，必须在本地运行 `cd src && cargo test`，确保单测保持 100% 通过（全绿）。
- **缺陷修复规范**：新缺陷修复需遵循项目的编号管理（从 #20 开始递增）。代码修复必须保证通用性，严禁针对特定任务或界面编写硬编码硬拦截逻辑。完成修复后，需使用今日头条断言局进行标准回归测试。
- **版本控制与提交**：`push` 权限属于用户。开发人员或 Agent 完成本地提交后，将提交哈希与变更概要呈报给用户，由用户决定执行推送操作。部署时使用 mv 进行原子的产物替换，防止进程踩踏。
- **汇报规范**：向用户呈报工作进度或结果时，要求提供最直接的技术事实、运行指标及对局结论（如：成功率、耗时分位数、Token 支出），禁止使用含糊夸张的非技术词汇，严禁在文档中堆砌虚假的性能或战绩宣称。

## 目录结构

```
src/                   Rust 内核源码目录
phonefarm.toml         控制参数、规则阈值及模型 Provider 回退链配置
docs/DESIGN.md         系统核心架构设计规范（记录契约、六步执行回路、数据隔离设计）
round.sh               单轮调度外壳脚本（保留兼容，生产环境建议使用 benchmark 命令）
ocr.swift / ocr        OCR 辅助文字识别模块
tasks/<任务名>/        任务数据目录（经验库 lessons.jsonl、转移图 tree.json、 campaign 汇总）
```

## 下钻分析流程

- 运行记录排查首选 `phonefarm last` 命令；定位到异常局 ID 后，通过 `show --step N` 定位到具体步骤上下文；使用 `cat ...stepN.xml.gz` 查看当时的原始 UI 树，进行离线解析与断言复盘。
- 局 ID 支持前缀模糊匹配。
- 严格遵守单步执行六拍回路（采集 -> 组装 -> 决策 -> 校验 -> 执行 -> 验收）。
- 项目的架构真理与设计逻辑被定义在 `docs/DESIGN.md` 中。任何核心功能升级、协议重写，必须先进行设计规格（SPEC）定义，并与已有架构保持一致。
