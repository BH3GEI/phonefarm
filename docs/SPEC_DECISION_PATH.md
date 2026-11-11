# SPEC: 决策原语通路 (typed decisions) — L1 契约

> 状态: 定稿 v1 · 本 PR 新增
> 来源: 协议形状与会话纪律移自 FluidUse 的决策模型运行时与 `PublishedCoreMLManager`
> (<https://github.com/FluidInference/FluidUse>, Apache-2.0, commit `9384e70`)。
> **只借语义, 不搬代码** —— FluidUse 是 Swift + Core ML, 本仓库是 Rust。
> 登记见 `THIRD_PARTY_NOTICES.md`。

---

## 0. 这条路现在能给什么 / 不能给什么

**能给**:

1. **一套协议与会话纪律**。把 Core ML 那一半接进来不用再动内核 —— 写一个说得来这套
   一行一个 JSON 的进程即可。
2. **一个零 Token 的降级档**。云端不可用时闭环继续跑。
3. **一颗能真跑起来的 Core ML 侧车**（`tools/decision-sidecar/`）：Swift + Core ML，
   带一个 137 字节的参考模型，`make build && make test` 就能把整条路验一遍。
   实测：同一组选项、两段不同的状态，选出了不同的东西（0.9~3ms/次，走 ANE）。

**不能给(别写成能)**:

- **参考模型不是模型**。它是「词面相关度打分器」：`scores[i] = dot(state, options[i])`，
  权重在运行时由输入给，语义完全由两侧的字符二元组哈希编码器决定。
  它不认识语义（换说法、指代、蕴涵、否定全不懂）。**它证明的是通路，不是质量。**
- **仓库不随附任何训练好的权重**。不下载、不构建。真模型（Laya / Kev / …）得你自己拿来，
  换法见 `tools/decision-sidecar/README.md`。
- **降级档的决策质量等同于随机。** 它的概率是均匀的 `1/n`，没有校准。
  它在「总比停摆强」这个意义上成立，不构成「省了钱而且没损失」。
  任何把它的概率当置信度用的代码都是错的。
- **它不做判定。** 好不好由真机 + 统计判定裁决，与 `AGENTS.md` 的硬边界一致。

---

## 1. 三种问题(typed decisions)

| 类型 | 问什么 | 答什么 |
| :--- | :--- | :--- |
| `choice` | 从 N 个选项里选一个 | 选中的 `label` + 概率 `p` |
| `score` | 在一个**有序**档位量表上打分 | 期望分 `expected`(0~1) |
| `noul` | 一个「是/否」的判断题 | 为「是」的概率 `p` |

请求:

```json
{
  "state": "客户为订单 #4471 被重复扣款，没有人回复。今天就要退款。",
  "questions": [
    {"type": "choice", "id": "want", "instructions": "客户想要什么?",
     "options": [{"label": "refund", "description": "退款"}, {"label": "status", "description": "查单"}]},
    {"type": "score", "id": "anger", "instructions": "客户有多生气?",
     "levels": ["calm", "annoyed", "angry"]},
    {"type": "noul", "id": "churn", "instructions": "客户会不会流失?"}
  ]
}
```

答案:

```json
{"source": "sidecar",
 "answers": [{"type": "choice", "id": "want", "selected": "refund", "p": 0.91},
             {"type": "score", "id": "anger", "expected": 0.8},
             {"type": "noul", "id": "churn", "p": 0.12}]}
```

**`state` 是一段文本, 不是一个界面。** 这条路不读屏、不截图、不碰设备 ——
把界面压成文本是调用方的事(见 `SPEC_OBSERVATION_BUDGET.md`)。

---

## 2. 请求校验: 不成立的请求不许发出去

结构上就不成立的请求在**发出去之前**拒掉, 不消耗一次侧车调用(更不消耗一次模型调用):

| 拒绝条件 | 理由 |
| :--- | :--- |
| `questions` 为空 | 没有问题要问 |
| `id` 为空 / 重复 | 答案对不回来 |
| `instructions` 为空 | 没有问什么, 模型只能瞎猜 |
| `choice` 的 `options` < 2 | 一个选项的「选择」不是选择 |
| `option.label` 为空 | 回一个空 label 等于没回 |
| `score` 的 `levels` < 2 | 一个档位的「打分」不是打分 |

---

## 3. 答案对齐: 保序 / 不臆造 / 不串型

侧车按自己的顺序回无所谓, 最终答案一律**按请求里问题的顺序**重排。三条硬规则:

1. **保序** —— 顺序由请求唯一决定, 不由侧车的回包顺序决定。
2. **不臆造** —— 少一条就是**整批作废**, 不用默认值补。半份答案比没有答案更危险:
   下游会以为那是完整的。
3. **不串型** —— 问 `choice` 回了 `score`, 是错。

---

## 4. 侧车协议与会话纪律

```text
  → {"op":"hello","protocol":1}
  ← {"op":"ready","protocol":1}
  → {"op":"decide","state":"…","questions":[…]}
  ← {"op":"answers","answers":[…]}
  ← {"op":"error","message":"…"}      (模型侧单次失败, 会话仍然可用)
```

`protocol` 对不上直接握手失败 —— 免得两边各说各话还以为通了。

会话纪律(照 FluidUse 的桥接层, 那些坑是真踩出来的):

| 规则 | 做法 | 为什么 |
| :--- | :--- | :--- |
| 一次一个, 按调用顺序 | 不交错写读 | 侧车是单进程单通道 |
| 启动超时独立于请求超时 | 握手上限 ≠ 单次推理上限 | 冷启动要编模型, 和推理不是一个量级 |
| 超时即杀会话 | SIGTERM → 等 1s → SIGKILL | 先给机会让它把模型句柄放掉 |
| **杀掉之后一律 `closed`** | 后续调用直接失败, **不重放** | 状态不明的请求重发比失败更坏 —— 与 `collect` 的不重放原则同源 |
| 模型侧错误不杀会话 | `{"op":"error"}` 只让这一次失败 | 一次推理炸了不等于要走重建流程 |
| stderr 单独收, 只留尾部 | 16 KB 环形缓冲 | 启动失败与超时要能把尾巴带出来 |

---

## 5. 资产校验(可校验、可复现)

`phonefarm decide --verify <目录> --manifest <清单.json>`:

```json
{"source": "FluidInference/laya-coreml@<revision>",
 "files": [{"path": "Laya.mlmodelc/weights.bin", "sha256": "…", "bytes": 123456}]}
```

空输出 = 通过。文件缺失、尺寸不符、SHA-256 不符各算一条问题, **不合并、不降级**。
换了 revision 就重新生成 manifest, 而不是「下下来就信」。

---

## 6. 命令行

```bash
phonefarm decide --local --seed 7 --request 请求.json   # 零 Token 降级
phonefarm decide --sidecar "你的桥" --request 请求.json  # 外部进程
phonefarm decide --verify 模型目录 --manifest 清单.json  # 只校验资产, 不推理
```

`--local` 的随机数走 `pyrandom`(CPython `random` 的逐位复刻):
**同一个请求 + 同一个 seed → 逐位相同的答案**。这是能写进证据的性质,
也是它敢自称「降级」而不是「随便给个值」的全部依据。

---

## 7. 尚未落地

- **训好的模型**。仓库不随附任何训练权重。现有的是参考模型（证明通路）+ 换模型的说明。
- **与 `llm.rs` 的接线**。`llm.rs` 已写明「失败必须可降级」, 但那条降级路径目前仍然
  走本地变异器; 把它改走 `decide` 需要先真机核一遍降级档的决策质量不比现状差 ——
  这一步没做, 所以**现在还没有任何一处生产路径在用它**。
- **校准**。`score` / `noul` 的概率口径只有真模型才谈得上, 参考模型没有。
- **并发**。侧车一次一个, `decide` 也是一次一个。要并发得在上层排队(或者开多个侧车)。
