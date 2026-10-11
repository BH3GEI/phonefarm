# 实验资格离线验证 — 2026-10-11

对应 [SPEC_EXPERIMENT_QUALIFICATION](SPEC_EXPERIMENT_QUALIFICATION.md)，跟踪
[组织 issue 12](https://github.com/HGamey/.github/issues/12)。

## 本轮实际执行

| 验证 | 结果 |
| --- | --- |
| `cargo test --manifest-path src/Cargo.toml --no-fail-fast` | 521 单测 + 7 binary CLI 集成全部通过 |
| `cargo clippy --manifest-path src/Cargo.toml --all-targets` | 完成；仓库仍有警告，本次修改的 Rust 文件无诊断 |
| `sh -n loop_v1/auto/sample_env.sh` | 通过 |
| `bash -n loop_v1/tools/run_once.sh` | 通过 |
| `git diff --check` | 通过 |
| 真机、模型/API 调用 | 无；Token 支出 0 |

## 有意义的反例

- 实际 eval/autoloop binary 经假 ADB 确认无 root 先停止，没有上传脚本、差异写探测或模型提名。
- 实际 probe_only binary 在快照不一致时返回 ABORT/blocked，不得 REVERTED。
- published synthetic 夹具在实际 CLI 返回 eligible=false；正例是假设 canonical 元数据，只验证逻辑。
- eval 离线 route 使用实际请求校验/目标冻结；即使资格逻辑通过也保持 qualification_only/ABORT，无优化 PASS。
- 缺任何必需指标、温度/来源/恢复未知或失败、上下两个方向的基线漂移均阻断结论。
- 只有一条合法采样、未完成、少采、压缩 uptime、无 trace 窗绑定、混入充电采样不满足整窗证据。
- 各轮负载退出码必须为 0；生成方不能放宽 alpha，较严 alpha 还要有足够统计可达性。

## 保留的真机门禁

还没有在本轮证明一加设备上的系统参数效果、完整真实采样窗、游戏触控重复性或性能改善。
KGSL 仍是 submit proxy；无直接 display collector。控制验证仅 probe/apply readback，不证明持续生效。
灰档控制面和连续帧质量没有独立真值，候选实验明确 ABORT。v1 shader/gpu-op 与旧 Python
编排尚未迁移到本合同。issue 12 保持开放，须真机验收后再决定是否完成。

## 2026-10-12 并发能力输出修正

保留上面的 `e411436` 历史验收。本次仅回补每调用私有输出目录和相应测试，
不引入独立确认 driver，也不改变资格规则或阈值。

| 验证 | 结果 |
| --- | --- |
| `cd src && cargo test --offline --no-fail-fast` | 521 单测 + 9 实际 binary CLI 全部通过 |
| `cd src && cargo clippy --offline --all-targets` | 完成；仓库仍有警告，本次修改的 Rust 文件无诊断 |
| 并发 fake ADB 输出交错 | 非 root 保持 ABORT/probe_allowed=false，不进入快照；另一 fake root 可通过该合成 probe 门禁 |
| 私有目录无法创建（TMPDIR 为普通文件） | root 检查 unknown、未调用 transport、未授权写入 |
| `git diff --check` | 通过 |

实际日志、源码及 binary 摘要在 `tasks/qualification_probe_output_20261012/`。
fake transport 与 root 正例仅为宿主回归夹具，不是设备认证；真机、模型/API 调用仍为 0。
