# phonefarm 架构参考

## 系统组成

- 内核：一个 Rust 二进制（main / runtime / brain / device / tree / fold / cli / telemetry）
- 文本文件：`phonefarm.toml`（配置+阈值+提示词+provider 链）+ `tasks/<任务>/` 数据目录
- 设计契约见 `docs/DESIGN.md`（记录契约/六步循环/任务隔离/写入权限）

## 目录

```
src/                   Rust 内核源码
phonefarm.toml         配置：阈值、提示词、provider 链（key 走环境变量）
docs/DESIGN.md         设计文档 v1（核心架构契约）
round.sh               旧版单轮脚本（仍可用；新流程用 benchmark 直跑）
ocr.swift / ocr        OCR 文字备胎（macOS Vision，首跑自动编译）
tasks/<任务>/
  lessons.jsonl        经验库（win/lose 计数，原子写回）
  tree.json            页面状态转移图（局末自动重算）
  campaign.tsv         评测账
  runs/<局ID>/         log.jsonl(账本) + ctx.log + stepN.jpg/.xml.gz（截图/树不入 git）
```

## 六步执行回路

采集（截图+UI树并行） → 组装上下文（goal+lessons+近5步+当前画面） →
模型决策（单次最多4动作） → 三道确定性门（值域/前科/空白点击） →
执行（换算坐标、等画面安静） → 验收（diff 差异，判定有无进展）。

出口：done→复核（正常） | 看门狗/预算（止损） | 服务全失败/设备故障（异常）。
出口后：时间点表 → 经验总结 → lessons.jsonl。

## 数据契约（log.jsonl 记录类型）

goal / screen / act / diff / note / lesson / ban / hook(verdict|arbit|heal|budget) /
raw / reflect / telemetry / app_event / trace / end

- 模型只许写 act 与 note，越权按格式错误处理
- note ≤200 字；lesson ≤20 条；act/diff 窗口 5 组
- `phonefarm schema` 输出完整字段契约

## 关键机制

- **探针**（inspect/find/get_state/history）：冻结世界加钟，向已有观测要细节，不重采画面
- **done 预检**（#21）：首次 done 不定局，经 alert 通道回显主张+当前前台，模型自纠一次
- **打摆检测**：反复 tap->back 或反复问同一问题 → 系统警告
- **遥测**：每步 68 字段快照，纯 r=telemetry 账本行，不进模型上下文
- **多设备并行**：`phonefarm parallel`，内核多线程，任务目录隔离；同任务并行默认拒绝

