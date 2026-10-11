# 连续配对帧质量门禁 v1

状态：离线实现；没有以合成夹具代替真游戏验收。对应 2026-09-17 真实 LR/HR 配对及
2026-09-24 连续画面色调/鬼影检测需求。测量与阈值属于 phonefarm，上层只提供候选。

## 命令与判定边界

```sh
phonefarm sequence-quality --reference reference.json --candidate candidate.json \
  --rules frozen_rules.json --out tasks/quality/report.json
```

全程零 Token、无设备调用。报告为 JSON，退出码 `0=PASS`、`1=FAIL`、`3=UNKNOWN`、
`2=输入/用法错误`。同样的文件与规则产生逐字节相同的 JSON；报告不写生成时间或绝对路径。
`PASS` **只表示所列帧、像素范围和三种指标在预登记阈值内**，不证明没有鬼影、不等于
用户观感验收、不证明性能/功耗改善，也不证明可部署到任何其他游戏。

判定顺序：证据完整性/配对可用性不成立 → `UNKNOWN`（即使某些帧指标很好）；配对成立
且至少一个指标越界 → `FAIL`；全部范围内 → `PASS`。`UNKNOWN` 不可进入“质量通过”归档。
参考的帧间变化超出运动门槛、静止序列没有足够运动覆盖、缺帧、时序错配、未验证对齐、
哈希不符、解码失败和尺寸变化均不按 PASS。输入结构非法则退出 2。

## 规则：在看候选之前冻结

规则 JSON `schema=phonefarm/sequence_quality_rules@1`，`protocol_id`，
`frozen_at_unix_ms`，以及以下 **显式必填** 阈值（无隐式默认）：

| 字段 | 单位/含义 |
|---|---|
| `min_frames` | 至少 3 帧；还必须有全部相邻转换 |
| `max_pair_timestamp_delta_ns` | 两臂同一登记时间线的最大配对偏差 |
| `max_frame_gap_ns` | 最大相邻采样间隔；0 不合法 |
| `max_alignment_error_px` | 已配准图像的误差上限；本工具不自动寻找“最好”的配准 |
| `min_psnr_db` | RGB 8-bit 编码值 PSNR 下限 |
| `max_channel_bias` | 每帧 RGB 各通道平均有符号偏差绝对值上限，0..255 |
| `max_temporal_residual_mae` | 相邻帧重建残差变化 MAE 上限，0..510 |
| `min_reference_motion_mae` | 整个序列至少一对的参考变化 MAE，避免静止夹具证明运动质量 |
| `max_reference_motion_mae` | 单对参考变化超过此值则本检测器的运动/配对可用性不成立 |

`exclude_rects` 为归一化 `[x0,y0,x1,y1]` 排除矩形数组，必须显式提供（可空）。
蒙版先于数据冻结，可排除 UI；不得看到异常后再隐藏问题区域。整幅排除非法。
规则原始文件的 SHA-256 必须写在两份 manifest 的 `rules_sha256`，规则 ID 必须相同。
声明的冻结时间必须早于两臂采集时间。哈希与时间声明能绑定材料，**不能独立证明时间戳
诚实或阈值确实提前登记**；正式案例还需 Git/issue/实验日志里的预登记记录。

## 配对 manifest

两份均为 `schema=phonefarm/paired_frame_sequence@1`。必填公共字段：
`protocol_id`、`rules_sha256`、`sequence_id`、`timeline_id`、`role`（reference/candidate）、
`captured_at_unix_ms`、`color_space=sRGB8`、`content_scope`、`source`、`frames`。
两臂 sequence/timeline/color/content scope 相同；时间戳必须在**同一个登记场景时间线**，
不能直接比较两次运行各自的设备时钟。图像已在相同空间对齐，本工具不缩放、不移动像素。

`content_scope` 枚举 `scene_only`、`full_frame_with_hud`、`masked_scene`。
`masked_scene` 必须有排除矩形。全帧含 UI 的指标只可称为全帧指标，不能称为纯场景质量。

`source` 必填：

- `kind`：`unknown`、`real_game_capture`、`synthetic_fixture`、`hr_downsampled`、`synthetic_hud_composite`。
- `provenance`：原始产物/采集日志的出处；禁止只写“真机”。
- `capture_method`：例如同帧 render-target dump；两次独立 screenshot 不天然成对。
- `device`、`package`、`game_version`：真游戏必填非空；其他来源也显式写 null 或具体值。
- `derivation`：降采样/合成 HUD 必填非空；真游戏直接采集或纯夹具写 null。

缺失 source 或 kind 按 `unknown` 处理，来源 UNKNOWN 不可 PASS。来源“真实”仍是
采集方的声明；哈希并不证明没有伪造。正式报告还需人工/采集系统审查 provenance。

两臂还必须绑定同一独立配对账本：`pairing_evidence={file,sha256}`。账本是
`schema=phonefarm/frame_pair_ledger@1`，含 `sequence_id`、`timeline_id`、`capture_method`、
`provenance`、`candidate_artifact_sha256` 及 `pairs`：每项 `frame_id,timestamp_ns,candidate_timestamp_ns,reference_sha256,candidate_sha256`。
`timestamp_ns` 为参考帧在登记时间线里的时刻，另一个字段绑定候选帧的时刻。
`capture_method` 可为 `same_frame_dual_output` 或 `deterministic_scene_replay`；后者只作为
声明的重放配对，不构成真实同帧采集。其他/缺失账本 → UNKNOWN。各条必须与两份 manifest
一一核对。只有同帧双输出账本且两臂声明真实同一设备/游戏/版本才有
`declared_real_game_source_pair=true`。账本由采集侧生成，不能由评测器给两份截图临时配 ID 后
宣称同帧。报告保留 `pairing_verification`，不会把 alignment 声明本身当成配对证明。
`pairing_verification=ledger_consistent` 只表示文件身份与索引一致，不自动证明两个画面
真的同一场景/时间。离线命令没有可信采集验证器，固定输出
`acquisition_attestation=not_checked`、`real_game_qualification=false`。正式游戏资格判定
需要独立采集系统证明或人工审查，不能只检查哈希和本命令的 PASS。
因此 `real_game_source_pair` 也固定 false，防止消费者把声明当成可信采集证明。
候选 manifest 必须有 `candidate_artifact={file,sha256}`，独立读取并计算该工件哈希，
与账本的 `candidate_artifact_sha256` 交叉核对；缺失或不匹配为 UNKNOWN，
`artifact_bound=false`。这使图像序列绑定到具体候选工件，仍不代表部署或采集真实性。

每帧必填 `frame_id`、`timestamp_ns`、`file`（相对 manifest 目录，禁止穿越/符号链接出界）、
`sha256`（文件原始字节）、`width`、`height`，以及 `alignment`：
`method=exact_pair|registered|unverified`、`reference_frame_id`、`error_px`、`evidence`。
参考臂也声明 identity 对齐。帧 ID 严格递增且相邻 +1，时间戳严格递增；必须两臂一一对应。
只支持 PNG/JPEG 解码后 RGB8，非 sRGB8/HDR 不自动转换。最大边 8192、每图最多
32M 像素、文件最多 64MiB、manifest 最多 10000 帧，避免损坏文件导致无界分配。

## 指标与解释

令已对齐、蒙版内的参考 `R_t`、候选 `C_t`，编码值范围 0..255：

- 空间：`MSE = mean((C_t-R_t)^2)`，`PSNR=10 log10(255²/MSE)`。完全相同以
  `exact_match=true, psnr_db=null` 表示无穷，不伪造 99 dB 或输出非有限 JSON。
- 色偏：`channel_bias = mean(C_t-R_t)`，各 RGB 通道独立；不是 DeltaE 或感知色彩度量。
- 连续：`E_t=C_t-R_t`，`temporal_residual_mae=mean(abs(E_t-E_(t-1)))`。它检测重建误差
  随时间的不稳定，可触发色调闪烁、合成拖影/滞后一类问题；**不是完整的鬼影检测器**。
- `reference_motion_mae=mean(abs(R_t-R_(t-1)))` 提供运动/配对可用性证据。超过冻结门槛
  记 UNKNOWN；本版本没有光流、遮挡处理或感知模型。

逐帧报告包含图像路径/哈希、配对差、尺寸、有效像素数、指标、越界原因；逐转换报告包含
前后 frame ID、参考变化和残差变化。异常可定位到原始帧重新检查。报告记录两个 manifest
和规则的文件哈希、来源类型、阈值、所有完整性问题与检测范围。

`declared_real_game_source_pair` 仅表示两臂来源**声明**为真实游戏、设备/包/版本相符且绑定同帧账本；不是游戏
验收标志。降采样或合成 HUD 必须保持其标签，哪怕其中用了真实截图。所有报告固定
`no_ghosting_proven=false`；合成测试结果不得作为真实游戏优化案例宣传。

## 验证与后续

单元/CLI 测试用程序生成 PNG 夹具，覆盖横竖屏、缺帧/时序错配/哈希失败/尺寸错误、
色偏、运动门槛、静止覆盖、合成拖影及可回放性。真机回来后采集同一帧的参考/候选 RT
序列，记录 UI 蒙版、版本与光照条件，先冻结规则再跑。插帧需要另定义生成帧的时间点和
匹配真值；本命令不把现有 SR 同帧契约冒充插帧验收。

已审计但不在本迭代改动：refbench `sr_pipeline` 的 compute `load_op=NONE` 与 schema
枚举冲突；灰层动态候选部署、真实 LR/HR 配对采集和完整感知/光流门禁仍为后续事项。
