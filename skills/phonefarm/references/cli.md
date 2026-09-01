# phonefarm CLI 参考

所有查看命令只读盘、不烧 token、支持 --json。局 ID 前缀模糊匹配，多命中列候选。

## run / benchmark / parallel

```bash
# 跑一局
phonefarm run --task <T> [--serial S] [--endless] [--budget-calls N] [--app P] [--assert \"a,b\"] \"<goal>\"

# 多轮评测（round.sh 已收编于此）
phonefarm benchmark --task <T> [--rounds N] [--budget-calls N] [--app P] [--assert \"a,b\"] [--json] \"<goal>\"

# 多设备并行
phonefarm parallel --job \"<task>|<goal>|<serial>|<app>|<assert>\" [--job ...] [--budget-calls N]
#   或多个 --serial 共享同一 goal/task：
phonefarm parallel --task <T> --budget-calls N --serial S1 --serial S2 \"<goal>\"
```

## 查看层

```bash
phonefarm last [--task T] [--json]          # 最近一局结论（入口）
phonefarm runs [--task T] [--limit N]       # 某任务全部局
phonefarm show <局ID> [--task T]            # 局概要：goal/动作/判定/文件清单
phonefarm show <局ID> --step N              # 单步：截图+els+全量+原始XML+telemetry
phonefarm show <局ID> --raw                 # 模型回包原文
phonefarm show <局ID> --hooks               # 系统判定（r=hook）
phonefarm show <局ID> --events              # 事件流（app_event：崩溃/ANR/fd增长/网络变化）
phonefarm show <局ID> --crashes             # 崩溃深度产物
phonefarm show <局ID> --anr                 # ANR trace
phonefarm show <局ID> --trace               # 系统 trace
phonefarm cat <路径> [--grep 词] [--tail N] [--head N]  # 万能打印：.gz解压/.jsonl美化/.jpg报尺寸
phonefarm stats <局ID>                      # 遥测汇总：帧率/CPU/内存/温度分位
phonefarm schema [--type R] [--markdown]    # log.jsonl 契约文档
phonefarm tree|lessons|campaign [--task T]  # 跨局产物
phonefarm tasks [--json]                    # 全部任务及统计
phonefarm config [--key k] [--json]         # 当前生效配置
phonefarm probe --serial S \"只读命令\"        # 设备只读直连
phonefarm exec --serial S \"命令\" --yes       # 设备任意命令（高危）
```

## 下钻链路

`phonefarm last` → `show <局ID> --step N` → `cat .../stepN.xml.gz`

