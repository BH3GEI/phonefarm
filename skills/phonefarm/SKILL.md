---
name: phonefarm
description: Drive the phonefarm mobile-device automation harness (Rust core + vision-language model). Use when operating Android emulators or OpenHarmony devices through the phonefarm binary: running or benchmarking automated UI-traversal sessions (run/benchmark/parallel), inspecting run results via the read-only CLI (last/runs/show/cat/stats/schema/probe), or working on the Rust codebase (src/runtime.rs, src/device.rs, src/cli.rs, src/telemetry.rs). Covers both Android (adb) and OpenHarmony (hdc) backends, telemetry collection, lesson/tree state, and the house rules for spending model tokens and pushing changes.
license: MIT
metadata:
  version: 1.0
  source-repo: github.com/BH3GEI/phonefarm
---

# phonefarm

手机自动化测试工具（Agent 自动化移动端并行、测试、采集工具）：Rust 内核驱动设备
（Android 模拟器 / OpenHarmony 真机），视觉语言模型看屏幕做决策，自动遍历 App 页面，
产出可复盘的完整记录。架构为执行回路与决策机制分离：模型只做屏幕识别与动作规划，
采集、校验、安全拦截、复盘、性能监控全部由确定性代码兜底。

## 何时用本 skill

- 需要跑一局（run）、多轮评测（benchmark）、多设备并行（parallel）时
- 需要查看某局结果（last/runs/show/cat/stats）、查账本结构（schema）时
- 需要改 Rust 内核（runtime/device/cli/telemetry）或加新能力时

## 关键命令速查

```bash
# 编译
cd src && cargo build --release && cp target/release/phonefarm ..

# 跑一局（Android）
./phonefarm run --task 今日头条遍历 --endless --budget-calls 90 --app com.ss.android.article.news \"<目标>\"

# 多轮评测
./phonefarm benchmark --task 今日头条遍历 --rounds 10 --budget-calls 90 --app com.ss.android.article.news --json \"<目标>\"

# OpenHarmony 设备
./phonefarm run --serial hdc:<connect key> --task OH设置冒烟 --budget-calls 30 \"<目标>\"

# 查看结果（只读盘，不烧 token）
./phonefarm last                                  # 最近一局结论
./phonefarm show <局ID> --step N                  # 单步钻取
./phonefarm show <局ID> --raw|--hooks|--events    # 模型原文/系统判定/事件流
./phonefarm stats <局ID>                          # 遥测汇总
./phonefarm cat <路径>                            # 万能打印
./phonefarm schema                                # 账本结构
```

## 必须守的规矩

- **跑局烧 GLM token（花钱）**。真机/长局前先向用户报价征得同意；单测、离线 CLI、编译随便跑。
- key 从 `./secrets.env` 自动读（`.gitignore` 排除，永不提交）。缺 key 程序会提示格式，不要替它编 key。
- 设备：Android 模拟器 AVD 名 `agentphone`；OH 真机在 intel-mac（ssh），需先同步仓库并重编译。
- 数据只写 `tasks/<任务>/` 之下：`log.jsonl` 只追加，`lessons.jsonl` 原子写，截图/原始 XML 不入 git。
- 改动后跑 `cd src && cargo test`，保持全绿（当前 49 项）。
- 修 bug：新缺陷从 #20 起编号，修法要通用不特调，修完用头条断言局回归验证。
- **push 归用户**，除非用户明确让推。部署用 mv 原子替换。
- 汇报讲人话，不堆黑话术语；README 不写战绩。

## 工作流提示

- 看结果先 `phonefarm last`，要细节再 `show --step N` → `cat ...stepN.xml.gz` 逐层下钻。
- 局 ID 打前缀即可，多命中会列候选。
- 每步六阶段：采集 → 组装上下文 → 模型决策 → 三道确定性门 → 执行 → 验收差异。
- 架构契约在 `docs/DESIGN.md`；新能力先出 SPEC 再实现。
- 多设备并行：`phonefarm parallel`（内核多线程，任务目录隔离；同任务并行会互踩 lessons/tree，默认拒绝）。

## 详细参考

- `references/architecture.md` — 架构、目录、数据契约、六步循环
- `references/telemetry.md` — 遥测十层字段与采集形态
- `references/cli.md` — CLI 全部子命令用法

