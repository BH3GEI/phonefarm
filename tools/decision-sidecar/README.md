# 决策侧车 —— 把 Core ML 决策模型接进 phonefarm

`phonefarm decide --sidecar <命令>` 需要的就是这个：一个说得来这套一行一个 JSON 的进程。
契约见 [`docs/SPEC_DECISION_PATH.md`](../../docs/SPEC_DECISION_PATH.md)。

```bash
make build && make test
phonefarm decide --sidecar "./build/decision-sidecar --model reference.mlmodel" --request 请求.json
```

只需要 macOS + Xcode 命令行工具。**不下载任何东西。**

---

## 先说清楚它到底是什么

**它是一台「词面相关度打分器」，不是 Laya，也不是任何训练过的模型。**

模型本体（`reference-model.py` 生成，137 字节）只有一个矩阵乘：

```
scores[i] = dot(state, options[i])      state: [256]   options: [32, 256]   scores: [32]
```

权重是**运行时由输入给的** —— 模型自己不说话，说话的是喂它的两个编码器。
侧车里用的编码器是：

```
文本 → 小写 → 字符二元组 → FNV-1a 哈希到 256 维桶 → L2 归一化
```

于是 `dot(state, option)` = 状态与选项在字符二元组空间里的余弦相似度。
这对「选项的词出现在状态里」这类问题是**真有信号**的：

```
案例 A  客户为订单 #4471 被重复扣款，没有人回复。今天就要退款。
        → choice = refund         (同一模型、同一组选项)
案例 B  登录之后一直白屏，重启 App 也没有用，我的订单页面打不开。
        → choice = technical_help
```

但它**不认识语义**：换说法、换语言、指代、蕴涵、否定，它全都不懂。

## 要拿到 Laya 那样的质量，换掉的是两端

决策头的形状不变，换的是编码器：

- 把二元组哈希换成真正的 transformer 编码器（在模型里加 encoder 层，或换成
  [Laya](https://huggingface.co/FluidInference/laya-coreml) 那样带编码器的 Core ML 包）；
- 或者整个换成一个已经训好的决策模型包。

换完仍然走同一条协议，`phonefarm decide` 一行都不用改。

### 换真模型怎么换

```bash
# 模型从哪来由你决定 —— 本仓库不随附、不下载、不构建任何训练权重
pip install coremltools huggingface_hub
huggingface-cli download <你的仓库> --local-dir model.mlpackage
xcrun coremlcompiler compile model.mlpackage .
phonefarm decide --sidecar "./build/decision-sidecar --model ./model.mlmodelc" --request 请求.json
```

`.mlmodel` 与 `.mlmodelc` 都收（前者在加载时现场编译，慢一点）。
计算单元用 `--units cpu|gpu|ane|all`，缺省 `all`（含神经引擎）。

---

## 协议

```
→ {"op":"hello","protocol":1}
← {"op":"ready","protocol":1,"model":"…","units":"…"}
→ {"op":"decide","state":"…","questions":[…]}
← {"op":"answers","answers":[…]}
← {"op":"error","message":"…"}      模型侧单次失败，会话仍然可用
```

会话纪律（保序 / 一次一个 / 超时即杀 / 杀掉即 closed）由 phonefarm 那一侧执行，
侧车只需保证「一行进、一行出」。

## 自检

```bash
make probe   # 用受控输入验模型图: 期望 [1, -1, 0]
make test    # probe + 三种题型各跑一次并打点
```

`--probe` 值得单独说一句：它拿 `state = e0`、`options = [e0, -e0, e1]` 去问模型，
正确答案只能是 `[1, -1, 0]`。**模型图和编码器是两件事** —— 这个项目的开发过程中，
「想要退款被判成技术支持」曾被误归到编码器头上，其实是先在 64 维桶上撞哈希撞没了。
没有这个自检，就会去改错的地方。

## 代码里三处踩过的坑（都留了注释）

| 坑 | 症状 | 处理 |
| :--- | :--- | :--- |
| 字符**三**元组 | 「要退款」与「退款」对不上（三元组要三字对齐，中文短词对不上） | 改**二元组** |
| **带符号**随机投影 | 短文本上碰撞噪声压过信号 | 改标准 hashing vectorizer（非负计数） |
| 64 维桶 | 中文二元组约 30 个/句，64 桶碰撞频繁，信号被淹没 | 加到 **256** 维 |

## 文件

| 文件 | 作用 |
| :--- | :--- |
| `sidecar.swift` | 侧车本体：CoreML 加载、编码器、三种题型解码、一行一 JSON 主循环 |
| `reference-model.py` | 生成参考模型（需要 coremltools） |
| `reference.mlmodel` | 生成好的参考模型，137 字节，已入库 |
| `Makefile` | `build` / `test` / `probe` / `model` / `clean` |
