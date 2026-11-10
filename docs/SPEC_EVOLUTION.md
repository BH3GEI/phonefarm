# SPEC: 项目内持续进化 — 假设—实验—证据闭环 (L1 契约)

> 状态: 定稿 v1 · 2026-09-14
> 来源: GitHub issue #11(基于代码快照 e20d850 的缺口清单)
> 范围: 先在移动端自动化场景验证。模型权重与宿主内核语义不变;项目内状态(解释/经验/能力/测量工具)的演进必须能实际改变下一轮行为,并能被独立评估。
> 预算纪律: 本 SPEC 的一切模型调用走 `[evolution]` 配额,不默认无限资源;实验运行前须声明样本规模与最小有意义提升。

---

## 1. 信任与证据语义(先裁定,后实现)

系统内一切"结论"必须能归类到以下四级证据来源之一,记录层显式标注,禁止混用:

| 级别 | 来源 | 语义 | 实例 |
|---|---|---|---|
| E1 | 执行者自述 | 模型对自己动作/结果的声称,永远不是证明 | act 的 text 依据、note、done_claim |
| E2 | 模型复核 | 另一个模型调用对材料的判读,可错("自信的错"),属建议 | verify 复核、arbit 仲裁、reflect 复盘 |
| E3 | 程序断言 | 确定性代码对采集材料的判定,在其量程内为强证据 | diff、assert 验收词实测、探针应答、校准通过的测量工具输出 |
| E4 | 外部裁判 | 系统外的终态事实,优先级最高 | aw-verdict.json、CTS 官方判分、人工标注 |

**冲突规则**:
1. 高级别覆盖低级别;E4 一旦存在即为终态(reflect 规则 4 已有,现提升为全局契约)。
2. 同级冲突或证据缺失时,结论状态为 `unknown`,不得包装成成功或失败二值。`r=end.achieved` 允许 `true/false/null`(null=unknown);旧数据二值语义不变(null 仅新增)。
3. 两种记录都保留:模型复核结论与外部结果各自落账,谁也不覆盖谁的字节。

**verify 输入偏差的正式裁定**: DESIGN §4 称复核"刻意排除执行者 note",当前实现(runtime.rs:2401-2433)包含 note 且标注"需与画面印证"。SPEC 裁定:**保留现状并正式收编**——复核输入含 note 是实测纠偏后的行为(排除曾产生误判),但 verify 的证据等级固定为 E2,verify 通过不再等于"任务达成",只等于"复核模型认为达成";任务达成与否以 E3/E4 为准,冲突时按规则 1-2 处理。DESIGN.md §4 措辞在下次文档修订时对齐本裁定。

**置信值契约**: 任何置信数字必须带来源种类,模型自报数字不得冒充校准概率:
- `{"kind":"freq","win":W,"lose":L}`: 由 E3/E4 结果累计,是唯一可数值比较的置信;
- `{"kind":"selfreport"}`: 模型自报,展示原样,不参与数值排序;
- `{"kind":"untested"}`: 尚无证据。

---

## 2. 数据模型(全部新增文件,既有文件零改动)

兼容策略:**旧 run/lessons/tree 数据原样可读,默认执行路径不受影响**。新状态全部落在任务目录下的新文件;旧二进制不认识这些文件但也不需要认识。文件首行恒为 `{"v":1}`,变更升版本。

### 2.1 hypotheses.jsonl — 解释的事件溯源存储(每任务,append-only)

假设(hypothesis)是对"项目/任务某现象为什么发生"的候选解释,生命周期以事件表达,**只追加,不改写**;当前状态由重放事件物化。

```json
{"v":1}
{"r":"hyp","op":"propose","id":"h7","q":"q1","t":"点击无响应是因为元素吸附到了错误坐标","conds":["设置列表页","元素列表可用"],"origin":{"run":"20260914-030000-1234-1","step":8,"code":"e20d850","model":"glm-5.3-flash","proto":"v1"},"conf":{"kind":"untested"}}
{"r":"hyp","op":"propose","id":"h8","q":"q1","t":"点击已生效但diff通道遗漏了变化","conds":["canvas类页面"],"origin":{...},"conf":{"kind":"untested"}}
{"r":"hyp","op":"support","id":"h7","ev":{"run":"...","step":9,"rec":"pred:p3"},"conf":{"kind":"freq","win":1,"lose":0}}
{"r":"hyp","op":"against","id":"h8","ev":{...},"conf":{"kind":"freq","win":0,"lose":1}}
{"r":"hyp","op":"retract","id":"h7","why":"反证:find探针确认目标元素不存在于全屏","ev":{...}}
{"r":"hyp","op":"supersede","id":"h7","by":"h9"}
{"r":"hyp","op":"narrow","id":"h9","conds":["仅Android 35设置列表"]}
```

- `q`(question) 标识竞争组:同一 q 下的活跃假设互为竞争解释;允许某 q 下没有任何活跃假设(=显式"不知道")。
- `op`: propose / support / against / retract / supersede / narrow(收窄适用条件)。
- 物化状态: candidate(propose 后) → active(有 freq 证据且未撤回) → retracted / superseded。retract/supersede 后不再注入,事件保留可审计。
- `origin` 五元组(run/step/code/model/proto)使每次更新可追溯到运行、步骤、代码版本、模型与数据配置;support/against 的 `ev` 指向具体预测或运行记录。
- support/against 事件只能由 **E3/E4 结果**触发(确定性代码落账),模型调用只能 propose/retract-建议,不得直接给自己记 support——这从机制上把 freq 置信锚定在客观结果上,回应"win/lose 由复盘模型自评、不能作独立证据"的缺口。

### 2.2 预测与检验(同在 hypotheses.jsonl)

```json
{"r":"pred","op":"register","id":"p3","hyps":["h7","h8"],"expect":{"h7":"diff_not_none","h8":"diff_none"},"test":{"kind":"probe","a":"find","q":"About"},"cost":1,"stop":"单次尝试","plan_b":"仍无结论则保持q1开放"},"at":{"run":"...","step":10}}
{"r":"pred","op":"outcome","id":"p3","observed":"diff_none","assert":"pass","revises":[{"id":"h8","op":"support"},{"id":"h7","op":"against"}]}
```

- **检验前必须登记**: 要区分的假设、各假设的可观察预测、成本、停止条件、结果如何影响后续行动。未登记的"检验"只是普通动作,不得事后追认为证据。
- `expect` 使用机器可判断言词表(封闭集合,见 §4.4),保证 outcome 链接是确定性代码判定,不引入模型自评进证据链。
- 结果三值: pass(观察到,按预测修订)/ fail(观察与预测相反)/ inconclusive(通道失效等无结论)——失败、无结论与负面结果全部保留,禁止只留成功轨迹。

### 2.3 capabilities.jsonl — 验证后固化的能力(每任务+_global,append-only)

```json
{"v":1}
{"r":"cap","op":"candidate","id":"c3","kind":"knowledge","t":"设置列表About项在列表最底部,需下滑3屏","scope":{"tasks":["设置-关于手机"],"conds":["列表可滚动"]},"from":{"hyp":"h9","runs":3},"eval":{"proto":"v1","win":3,"lose":0},"ver":1}
{"r":"cap","op":"adopt","id":"c3","ver":1,"at":{...}}
{"r":"cap","op":"rollback","id":"c3","ver":1,"why":"新初态下连续2次失效","ev":{...}}
```

- kind: knowledge(检索知识,注入上下文)/ skill(结构化技能,动作宏)/ tool(可执行测量工具,见 §5)。
- 生命周期: candidate →(候选评测有收益)→ adopted →(退步/失效)→ rolledback;再启用须升 ver 重新走候选。
- 每项带来源(from)、适用范围(scope)、评测结果(eval,含评分协议版本 proto)与版本(ver)。
- 与 lessons 的关系: lessons.jsonl 维持现状(操作建议层);capabilities 是经过假设层证据筛选、带版本与回退的固化层。注入优先级: adopted capability > lesson;两者都在 render_ctx 标注来源。

### 2.4 tools.jsonl — 候选测量工具登记(全局,append-only)

```json
{"v":1}
{"r":"tool","op":"propose","id":"m2","kind":"logrep","def":{"pattern":"ANR in","in":"logcat"},"out_schema":{"match":"bool","detail":"str"},"origin":{...}}
{"r":"tool","op":"calibrate","id":"m2","samples":[{"run":"...","label":"fail","got":"fail"},{"run":"...","label":"ok","got":"ok"}],"errors":0,"limits":"仅对含logcat的局有效"}
{"r":"tool","op":"adopt","id":"m2"}
{"r":"tool","op":"retire","id":"m2","why":"误报率2/5超过阈值"}
```

- 校准门槛(默认,可配): 已知标注样本 ≥4(成功/失败/无变化三类至少各 1),误判 ≤1 且无误报成功。**校准不通过的工具不得进入正式反馈路径**(验收场景 5 的硬要求)。
- 工具输出属 E3(程序断言),但标注 `by:tool:<id>@<ver>` 以便撤回其全部历史结论。

---

## 3. 闭环机制:失败 → 解释 → 检验 → 修订 → 采用

### 3.1 触发(确定性,不进模型)

局内出现以下信号之一且预算有余时,运行时在**下一步边界**触发一次 hypothesize 调用:
- T1 重复失败: 同一动作目标(what 或坐标区域)第二次被驳回或 diff=none;
- T2 done 复核未通过(E2)且官方判分缺失或为 fail;
- T3 存在已登记未执行的检验(pred register 无 outcome,如中断恢复后)。

每局限额: hypothesize 调用 ≤`evo_hyp_calls`(默认 1),检验动作 ≤`evo_test_actions`(默认 2)。配额用盡则信号仅落账(事件不丢),局末复盘仍可处理。

### 3.2 hypothesize 调用(E2,纯文本,不进图像)

新增提示词 `hypothesize`(phonefarm.toml [prompts]),输入: 失败上下文(相关 act/diff/rejected 行, probe 应答)、当前竞争组物化视图、可用探针/断言词表。输出 JSON:

```json
{"q":"q1","hyps":[{"id":"h7","t":"...","conds":["..."]},{"id":"h8","t":"...","conds":["..."]}],
 "test":{"kind":"probe","a":"find","q":"About",
         "expect":{"h7":"diff_not_none","h8":"diff_none"},
         "cost":1,"stop":"单次尝试","why":"find能区分吸附错误与通道遗漏"}}
```

规则(写进提示词): 必须给 ≥2 个互斥或可区分的解释;每个解释的可观察预测必须不同;检验优先只读探针;模型不得声称概率。运行时对输出做 schema 校验,不合法即丢弃落账(不进入证据链)。

### 3.3 检验执行(复用既有闸门)

- kind=probe: 走现有探针通道(runtime.rs:753-846),只读零副作用,连击计数独立于普通探针但仍受每局限额。
- kind=act: 仅允许 wait/scroll 类低风险动作;tap 类副作用动作 v1 不开放给检验(检验的目的是区分解释,不是冒进)。全部经过既有三道执行前检查。
- 执行前后: register 事件先于动作落账;动作照常有 act/diff 记录。

### 3.4 结果链接(确定性,E3)

pred 的 `expect` 断言词表(封闭): `diff_none` / `diff_not_none` / `probe_found` / `probe_not_found` / `probe_ans_contains:<词>` / `rejected` / `not_rejected`。
运行时按词表比对实测(diff 串、probe 应答),产出 pass/fail/inconclusive,生成 support/against 事件。**任何一步失败都不得中断局**——检验失败本身是数据。

### 3.5 采用(下一轮行为改变)

- render_ctx 新增静态段(置于 lessons 之后,保持前缀缓存): 当前任务活跃假设 ≤`evo_inject_max`(默认 6 条),按 q 分组,各带 conf 标注;retracted/superseded 不注入;注入文本明示"假设,待证,与画面矛盾时以画面为准"。
- 适用条件匹配: `conds` 与当前页面上下文(activity/页面身份证)粗匹配,不匹配的假设不注入——避免全量历史堆进提示词。
- reflect 提示词追加一段: 本局 pred outcome 摘要作为复盘材料,lesson 的新增/修订可引用假设 id;lesson 的 win/lose 语义不变(兼容),但 freq 证据以 hypotheses 层为准。
- 跨任务传播: 全局假设放 `tasks/_global/hypotheses.jsonl`,注入时带 from 溯源;提供 `phonefarm hyp --isolate/--disable` 开关(隔离、禁用与回退能力)。

### 3.6 时间点表挂载(符合 DESIGN §1 新能力标准)

| hook | 触发 | 性质 | 产出 |
|---|---|---|---|
| `on="hypothesize"` | T1/T2/T3 信号且配额有余 | 模型调用(hypothesize 提示词) | hypotheses.jsonl 事件 + pred register |
| `on="pred_outcome"` | 检验动作 diff/probe 应答落地 | builtin(确定性) | pred outcome + hyp support/against |
| `on="episode_end"` | 局末 | 既有 reflect 扩展材料 | lessons(现状)+ 假设压缩(超限合并由模型建议、代码落账) |

---

## 4. 测量工具演进(§2.4 的执行面)

1. **工具形态 v1(声明式,不开放任意代码)**: `logrep`(对局内日志/logcat 的正则探针)与 `xmlassert`(对 stepN.xml 的元素存在断言)。输入只读局目录,限时 5s,输出必须符合 out_schema `{match:bool, detail:str}`。复用设备操作约束,不获得裸设备写权限。
2. **提议入口**: hypothesize/reflect 输出可附 tool 提议;CLI `phonefarm tools --propose <def.json>` 亦可人工登记。提议只是 candidate,不自动生效。
3. **校准管线**: `phonefarm tools --calibrate <id>` 在标注样本集(来自历史局的 E3/E4 标签与人工补充标签,存 `tasks/_global/calibration/<tool-id>.jsonl`)上跑,产出误判与适用限制,达标后 adopt。
4. **使用**: adopted 工具作为新探针种类挂入 probe 注册点(runtime.rs:1038 PROBES),输出按 E3 注入与落账,带 `by:tool:<id>@<ver>`。
5. **退役**: retire 事件使其立即停用;其历史结论在审计查询中标注"来源工具已退役"。

## 5. 能力固化(§2.3 的执行面)

- **候选生成(自动,确定性)**: 假设满足 win≥3 且 lose=0 且跨 ≥2 个不同 run 有证据 → 生成 knowledge 类 candidate(事件自动落账,可配置关闭)。
- **启用闸门**: candidate 须经评测(experiment 或 eval 命令,proto 版本固定)显示有收益才 adopt;v1 允许人工 `phonefarm caps --adopt <id>`(人工评测结论须随 adopt 事件落账)。
- **回退**: rollback 事件即刻停止注入;再启用升 ver 重新候选。全部历史可 `phonefarm caps` 审计。
- skill/tool 类能力的候选评测复用同一闸门;skill(动作宏)的执行走 UniversalEngine 0-token 短路点(runtime.rs:1856-1926 的插件拦截位)。

## 6. 中断恢复(不丢证据、不重复记账、副作用不明先核对)

1. **证据**: hypotheses/capabilities/tools 均 append-only,崩溃不丢已落账事件;log.jsonl 本就只追加。
2. **不重复记账**: pred outcome / support / against 事件携带 `ev.rec` 幂等键(pred id 或 run+step),物化时按键去重;局末处理以 run id 幂等。
3. **恢复核对(resume 语义 v1)**: 提供 `phonefarm run --resume <局ID>`:
   - 账本最后一条 act 无对应 diff → 该动作执行状态 **unknown**:恢复后第一步固定为只读采集(screen+get_state),与账本预期比对,落 `{"r":"hook","kind":"resume_check","last_act":...,"observed":...,"consistent":true|false}`;consistent=false 时不重放任何副作用动作,把现状交还模型决策。
   - 中断局存在的 pred register 无 outcome → 按 T3 信号重新调度检验,不重复 register。
4. 长程调度(实验/多轮)的暂停/恢复/停滞/终止见 §7.6。

## 7. 实验框架(A/B/C 对比)

### 7.1 臂定义(固定基础模型、工具权限与可比调用预算)

- **A 臂**: 无跨局经验(lessons/hypotheses/capabilities 注入全部关闭,局内机制照常);
- **B 臂**: 冻结经验(实验启动时对 lessons/hypotheses/capabilities 做只读快照,全程不变);
- **C 臂**: 持续演进(现状全量机制)。

### 7.2 隔离

- 每臂独立状态根: `tasks/_exp/<exp-id>/<arm>/`(经 `PF_TASKS_ROOT` 钉住),互不读写;
- 适应任务与保留评测任务分离;评测时冻结候选状态(`--freeze-state`),评测结果不得回流学习;
- 设备初态: 实验 spec 声明目标应用版本与复位程序(自建测试应用允许 `pm clear`;第三方应用声明无法控制的因素并记录);
- 顺序效应: 各臂同任务交错排程(A1B1C1,A2B2C2…),随机种子与排程序列入报告。

### 7.3 评分协议

- `[scoring] protocol_version` 显式版本;实验 spec 钉住版本,运行中变更直接拒绝;
- 评分依据(验收词/官方判分/协议文本)随 spec 归档;协议升级需独立评审,报告保留新旧口径对照;
- 生成者(被评的演进机制)不得修改评分依据、参考答案或历史结果:实验目录下评分材料写后只读,报告引用其哈希。

### 7.4 报告(可复查)

`tasks/_exp/<exp-id>/report.md` + `report.json`:
- 样本数(运行前声明)、逐任务逐局结果、总体与分项成功率、波动/不确定性(逐局 p 值或 bootstrap 区间,样本<10 时只报原始分布不强行推断);
- 费用(calls/tokens/分 provider)与耗时(wall 分位数);失败成本单列;
- 预测准确性(pred pass 率)、无效尝试率(被驳回+空击占比)、新条件泛化(保留任务成功率)三组机制指标——不得以经验条数、反思长度或自评 win 代替;
- 消融臂: `--ablate no-active-testing`(关 §3 检验)/ `--ablate no-cap-screening`(关 §5 闸门),确认收益来源;
- 无法控制的因素如实列出。**无提升或退步即交付负结果与解释**;"机制已实现"与"收益已验证"分开陈述。

### 7.5 命令形态

```
phonefarm experiment <spec.toml> [--arm A|B|C] [--ablate ...] [--resume]
```

spec.toml(示例关键字段): `id`、`arms=["A","B","C"]`、`adapt_tasks=[...]`、`eval_tasks=[...]`、`rounds=N`、`budget_calls=N`、`token_cap=N`、`scoring_proto="v1"`、`min_meaningful_delta=0.15`、`app_reset={pkg="...","clear=true}`。

### 7.6 长程调度

- 预算: token_cap 硬顶,触顶停臂并落账;每局 budget_calls 如常;
- 暂停/恢复: 进度台账 `tasks/_exp/<exp-id>/ledger.jsonl`(append-only,每局一行含臂/任务/结果/费用),`--resume` 从台账续跑,已完成局不重复记账;
- 停滞: 单臂连续 N(spec 声明,默认 3)局设备级失败(非任务失败)→ 停臂标记,其余臂继续;
- 终止: 全部臂完成 rounds 或全部停臂;报告只在终止后生成(支持 `--report-only` 重生成)。

## 8. 训练数据导出与模型评估接口

### 8.1 导出

```
phonefarm export [--task T] --split train|heldout --out <文件> [--redact-config <toml>]
```

- 数据源: runs 的 raw 记录 + ctx.log(模型视角完整存档);逐条带 provenance `{run, step, code, model, proto, task, split}`;
- 分割: `tasks/_exp/evalsets.toml` 声明 heldout 局/任务集合;`--split train` 导出时 heldout 集合一律排除(硬过滤,宁可少导);
- 脱敏: secrets 本就不入账;屏幕隐私按 redact-config(包名/关键词正则)抹除文本与截图引用;私密屏幕内容、原始密钥、heldout 材料混入导出视为事故,导出器自检(抽样比对)后报告抹除计数;
- 格式: JSONL,首行 `{"v":1,"kind":"pf-export","proto":"..."}`。

### 8.2 模型评估接口

```
phonefarm eval --set <evalset.toml> --candidate <caps快照|config路径> --out <json>
```

- 版本化评测集(evalset.toml 带 ver 与任务/局引用);评测协议版本固定;
- 候选可以是能力快照(评 §5 的启用闸门)或整份配置(评模型替换);
- 模型替换类候选须同时报: 目标能力、未参与训练的新任务、原能力回归三组指标;不允许仅凭训练损失下降替换(本期不训练,接口先行)。

## 9. CLI/MCP 面汇总

新 CLI(全部 --json): `hyp`、`pred`、`tools`、`caps`、`experiment`、`export`、`eval`。
MCP 新增只读工具: `phonefarm_hyp/pred/tools/caps` 与 `phonefarm_experiment_report`;experiment/export/eval 不进 MCP(写操作与长程任务,CLI 人工驱动)。cat 监狱不变。

## 10. 配置([evolution] 段)

```toml
[evolution]
enabled = true
hyp_calls_per_episode = 1      # hypothesize 调用/局
test_actions_per_episode = 2   # 检验动作/局
inject_max = 6                 # 假设注入上限
auto_cap_candidate = true      # 达阈值自动生成能力候选
tool_calib_min_samples = 4
tool_calib_max_errors = 1
[scoring]
protocol_version = "v1"
```

关闭 `enabled` 全部新机制静默旁路(零行为变化)——这是"默认既有执行路径不被破坏"的总开关。

## 11. 验收场景 → 机制映射

| 验收场景 | 机制 | 验证方式 |
|---|---|---|
| 相同表面失败不同真实原因 | §3 竞争解释+预测登记+结果修订 | 注入两种初态的点击失败局,查 q 组多假设与 outcome 修订链 |
| 注入错误经验被限制/撤回 | §2.1 retract + freq 反证 | 人工 propose 错误假设,跑反证局,查 retract 事件与不再注入 |
| UI 变化但任务未完成不得判成功 | §1 E2≠达成 + verify 冲突规则 | done 但 assert 未命中的局,achieved 不得为 true |
| 模型自评成功外部失败,双记录保留 | §1 规则 3 + aw-verdict 链路 | 投放 aw-verdict fail 的局,verify true 与官方 fail 同存,下轮上下文含官方判分行 |
| 误报工具不进正式反馈 | §2.4 校准门槛 | 构造在失败样本误报的工具,校准拒绝 adopt |
| 有用经验跨初态/新任务独立验证 | §5 候选闸门 + §8.2 eval | candidate 在 heldout 任务 eval,adopt/rollback 可追踪 |
| 中断恢复不丢证据不重复记账 | §6 resume_check + 幂等键 | 杀掉进程恢复,查事件无重复、resume_check 落账 |
| A/B/C 隔离与可复查报告 | §7 | 跑最小实验(声明样本量),查三臂状态根隔离与报告字段 |
| 旧数据兼容 | §2 兼容策略 | 全量单测 + 用旧 tasks/ 数据跑 last/show/lessons 不报错 |
| 训练导出来源/分割/脱敏 | §8.1 | 导出文件查 provenance/split,heldout 局 grep 不到,脱敏计数报告 |

## 12. 交付顺序(对齐 issue 建议)

1. 本 SPEC + 核对清单(本文件即交付物)。
2. 最小贯通切片: §1 冲突规则(unknown 三值) + §2.1/2.2 假设与预测存储 + §3 闭环(T1 触发+probe 检验+结果链接+下轮注入) + 单测。
3. §4 测量工具 + §5 能力固化 + §6 中断恢复 + 审计 CLI(hyp/pred/tools/caps)。
4. §7 实验框架与 10 项验收;A/B/C 真实设备实验(预算另行授权)。
5. §8 导出与 eval 接口;实际训练另按资源推进。

## 13. 非目标

- 不改动模型权重;不引入 rmcp/tokio 之外的新重型依赖(hypothesize 走既有 brain 调用链)。
- v1 不开放任意代码型测量工具、不开放 tap 类副作用检验动作、不做跨设备知识合并。
- 不以本 SPEC 宣称收益已实现:机制实现与收益验证分开交付,负结果如实上报。
