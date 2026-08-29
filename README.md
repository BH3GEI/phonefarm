# phonefarm

让 AI 自己玩手机:一个自动遍历 Android App 的 agent。

Rust 内核驱动 Android 模拟器(adb),视觉语言模型(智谱 GLM)做决策,跑一个六步循环:**采集屏幕 → 拼材料 → 模型决策 → 执行前检查 → 执行 → 验收**。目标是不用人管,把一个 App 的全部功能页面自己逛一遍,并产出覆盖清单。

## 它靠什么活着

- **双通道感知**:UI 元素树(文字+坐标,主通道)+ 截图像素对比(兜底);"等画面安静"采样 + 背景噪声扣除,能正确对付视频自动播放、动画等永不静止的画面
- **信任门**:空白点击驳回、同一坐标连点驳回、未知动作驳回、预算看门狗;像素差异拿不准时由视觉模型仲裁
- **计划链**:一次决策最多产出 4 个动作,后续动作不再消耗模型调用(实测免调用步占 26~36%)
- **经验沉淀**:每局结束复盘,把教训写进 `tasks/<任务>/lessons.jsonl`(带 win/lose 计数),下一局开场即用;代码层兜底防止复盘静默丢条
- **独立复核**:局末用终局画面 + 差异记录交叉验证 agent 的"完成"主张——实测抓住过一次谎报
- **韧性**:provider 链式回退、两级限流冷静期(45s+90s)、内容审核拒图时"盲滑逃离"、模拟器死机自动重启;`done` 只许单独发出,宣判前必须重新看过屏幕

## 目录

```
src/                  Rust 内核(main / runtime / brain / device)
phonefarm.toml        阈值、提示词、provider 链配置(密钥走环境变量)
round.sh              单轮端到端:设备体检→清应用→跑一局→汇总一行账
summarize_run.py      从 log.jsonl 抽取单局汇总
tasks/<任务>/          各靶子的经验库 lessons.jsonl、逐局 runs/(log.jsonl 入库,截图不入库)
phonefarm-设计文档-v1.html
```

## 跑起来

1. `cp secrets.env.example secrets.env`,填入智谱 key(Coding 套餐)
2. `cd src && cargo build --release`,把产物 `phonefarm` 放到仓库根目录
3. 启动 Android 模拟器(AVD 名 `agentphone`),装好目标 App
4. 单轮:`./round.sh 1`;或直接:
   `./phonefarm run --task "今日头条遍历" --endless --budget-calls 90 "<目标文本>"`

## 战绩(2026-08-29)

今日头条全功能遍历,10 轮端到端连跑:**5 轮"完成+复核"双过,最后三轮连续通过**;共 442 步、311 次模型调用、76 分钟;全程暴露并修复 6 个真实缺陷(便签当动作、复盘丢经验、模型图像通道劣化、内容审核拒图、back+done 连发、限流误判)。明细见 `tasks/今日头条遍历/campaign10.tsv` 与各局 `log.jsonl`。
