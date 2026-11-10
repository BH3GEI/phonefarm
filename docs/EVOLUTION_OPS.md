# 持续进化机制操作说明 (issue #11)

对应 SPEC: `docs/SPEC_EVOLUTION.md`。本文档是日常使用与审计的操作面。
所有机制默认开启;`phonefarm.toml` 的 `[evolution] enabled = false` 可整体旁路,
行为与旧版完全一致(零扰动承诺)。

## 概念一分钟版

局内发生重复失败(T1)或 done 复核未达(T2)时,系统让模型给出 2~4 个**竞争解释**
和一份**能区分它们的检验**;检验预测先登记(pred register),执行后由**确定性代码**
对照封闭断言词表链接结果(E3 证据),支持/反对计数只从此处来——模型不得给自己记功。
证据达标的假设在局末自动固化为**能力候选**,经评测启用后注入后续决策;
候选测量工具经校准(误报成功一票否决)后挂入探针注册点。

## 存储(全部 append-only 事件溯源,崩溃不丢账)

| 文件 | 内容 | 首行 |
|---|---|---|
| `tasks/<T>/hypotheses.jsonl` | 假设与预测检验事件(本任务) | `{"v":1}` |
| `tasks/_global/hypotheses.jsonl` | 跨任务通用假设 | `{"v":1}` |
| `tasks/<T>/capabilities.jsonl` / `_global` | 能力固化事件 | `{"v":1}` |
| `tasks/_global/tools.jsonl` | 测量工具事件(全局一份) | `{"v":1}` |
| `tasks/_global/calibration/<tool-id>.jsonl` | 人工校准样本集 | 无 |

物化 = 重放事件,不原地改写;support/against 按 `(pred id | hyp id | op)` 三元组幂等去重,
中断重试不重复记账。

## CLI 速查(全部支持 `--json`)

```bash
phonefarm hyp [--task T]                      # 假设物化视图(候选/活跃/已撤回/已被替代)
phonefarm hyp --retract <id> --why "<反证>"    # 撤回(不再注入,事件留痕)
phonefarm hyp --supersede <id> --by <新id>     # 替代(替代链可追责)
phonefarm pred [--task T]                     # 预测检验台账(登记→检验→结果)
phonefarm caps [--task T]                     # 能力列表(任务域+全局域)
phonefarm caps --adopt <id> [--eval "<评测结论>"]  # 人工启用(结论随事件落账)
phonefarm caps --rollback <id>                # 回退(即刻停注)
phonefarm tools                               # 测量工具列表
phonefarm tools --propose <def.json>          # 登记工具提议(亦可内联 JSON)
phonefarm tools --calibrate <id>              # 跑校准,达标自动启用
phonefarm tools --retire <id>                 # 退役(终态)
phonefarm export [--task T] --split train|heldout --out <文件> [--redact-config <toml>]
phonefarm eval --set <evalset.toml> --candidate <caps快照|config> --out <json>
phonefarm run --task T --resume <局ID前缀> [其他run参数] (goal 可省略=继承旧局)
```

MCP 只读面同步新增:`phonefarm_hyp / phonefarm_pred / phonefarm_caps / phonefarm_tools`
(serve 模式;experiment/export/eval 不进 MCP,CLI 人工驱动)。

## 工具定义(v1 声明式封闭词表)

```json
{"kind":"logrep","def":{"pattern":"ANR|FATAL"}}
{"kind":"xmlassert","def":{"text":"必现词","text_absent":"禁现词"}}
```

- logrep:`|` 分隔多词任中(contains 实现,非正则引擎);
- xmlassert:必现+禁现双查当屏全量文字层;
- 输出固定 `{match:bool, detail:str(≤80字)}`,注入时带 `by:tool:<id>@<ver>` 标注;
- 校准样本:`{"input":{"kind":"log|xml","text":"..."},"label":"match|no_match"}` 每行一条,
  门槛 v1:样本≥3、误判=0、误报成功(no_match 判成 match)=0(一票否决);
- 已启用工具会以 `tool:<id>` 动作名出现在模型探针词表(上下文有"测量工具"段时)。

## 中断恢复(--resume)

账本最后一条 act 无 diff/probe = **执行状态不明**:恢复局第一步只读采集核对,
落 `hook kind=resume_check {consistent}`;consistent=false 不重放任何副作用,
把现状交还模型。pending pred 由 T3 重新调度,不重复 register。

## 导出与评估

- `export` 把 raw(模型回包)与 ctx.log(决策上下文)按步配对为 (输入,输出) 样本,
  带 provenance `{task,run,step,model,proto,split}`;
  `tasks/_exp/evalsets.toml` 声明 heldout(任务名/局ID前缀),train 导出硬排除;
  写后自检 heldout 混入与 `sk-` 密钥模式,命中即事故非零退出。
- `eval` 当前支持:caps 快照离线证据账(逐能力 eval/from);config(模型替换)为协议壳
  (target/novel/regression 三组指标,执行器接入后填充)。

## 排障

| 现象 | 排查 |
|---|---|
| 假设不注入 | `hyp` 查状态(撤回/替代不注入);conds 是否匹配当前 activity/page;`inject_max` 上限 |
| 检验不调度 | `pred` 查 pending;page_hint 与当前 activity 是否前缀匹配;`test_actions_per_episode` 配额 |
| 工具调用被拒 | `tools` 查状态(仅 Adopted 可用);退役为终态须重新 propose |
| 能力不出现 | 自动候选阈值 win≥3 且 lose=0 且跨≥2 局;`caps` 查是否已在候选 |
| 想关掉全部 | `[evolution] enabled = false` |
