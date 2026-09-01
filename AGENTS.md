# phonefarm

Agent 自动化移动端并行、测试、采集工具：Rust 内核驱动设备（Android 模拟器 / OpenHarmony 真机），
视觉语言模型看屏幕做决策，自动遍历 App 页面，产出可复盘的完整记录。
核心思路是弱模型 + 强骨架：模型只负责"看屏幕、决定下一步"，采集、校验、防卡死、记账、性能监控全由确定性代码兜底。

## 常用命令

```bash
# 编译
cd src && cargo build --release && cp target/release/phonefarm ..

# 跑一局（Android）
./phonefarm run --task 今日头条遍历 --endless --budget-calls 90 --app com.ss.android.article.news "<目标文本>"

# 多轮评测
./phonefarm benchmark --task 今日头条遍历 --rounds 10 --budget-calls 90 --app com.ss.android.article.news --json "<目标文本>"

# OpenHarmony 设备（hdc 走 intel-mac）
./phonefarm run --serial hdc:<connect key> --task OH设置冒烟 --budget-calls 30 "<目标>"

# 查看结果（CLI 即入口，只读盘不烧 token）
./phonefarm last                                  # 最近一局结论
./phonefarm runs [--task T]                       # 某任务全部局
./phonefarm show <局ID> [--step N]                # 局概要 / 单步钻取
./phonefarm show <局ID> --raw|--hooks|--events    # 模型原文 / 系统判定 / 事件流
./phonefarm stats <局ID>                          # 遥测汇总（帧率/CPU/内存/温度）
./phonefarm cat <路径>                            # 万能打印（.gz/.jsonl/.jpg）
./phonefarm schema                                # log.jsonl 全部记录类型
./phonefarm probe --serial S "只读命令"           # 设备直连调试
```

## 必须守的规矩

- **跑局烧 GLM token（花钱）**。真机/长局前先向用户报价征得同意；单测、离线 CLI、编译随便跑。
- key 从 `./secrets.env` 自动读（`.gitignore` 已排除，永不提交）。缺 key 时程序会提示格式，不要替它编 key。
- 设备：Android 模拟器 AVD 名 `agentphone`；OH 真机在 intel-mac（ssh），仓库需同步并重编译后再跑。
- 数据只写 `tasks/<任务>/` 之下：`log.jsonl` 只追加，`lessons.jsonl` 原子写，截图/原始 XML 不入 git。
- 改动后跑 `cd src && cargo test`，保持全绿。
- 修 bug 按项目惯例：新缺陷从 #20 起编号，修法要通用不特调，修完用头条断言局回归验证。
- **push 归用户**，除非用户明确让推。部署用 mv 原子替换。
- 汇报讲人话，不堆黑话术语；README 不写战绩。

## 目录

```
src/                   Rust 内核（main/runtime/brain/device/tree/fold/cli/telemetry）
phonefarm.toml         配置：阈值、提示词、provider 链（key 走环境变量）
docs/DESIGN.md         设计文档 v1（记录契约/六步循环/任务隔离/写入权限）
round.sh               旧版单轮脚本（仍可用；新流程用 benchmark 直跑）
ocr.swift / ocr        OCR 文字备胎（macOS Vision，首跑自动编译）
tasks/<任务>/          lessons.jsonl(经验) tree.json(交互网) campaign.tsv(评测账) runs/<局ID>/(账本+截图+原始树)
```

## 工作流提示

- 看结果先 `phonefarm last`，要细节再 `show --step N` → `cat ...stepN.xml.gz` 逐层下钻。
- 局 ID 打前缀即可，多命中会列候选。
- 每步六拍：采集 → 组装上下文 → 模型决策 → 三道确定性门 → 执行 → 验收差异。
- 设计文档是架构真理，改架构先读 `docs/DESIGN.md`；新能力先出 SPEC 再实现。
