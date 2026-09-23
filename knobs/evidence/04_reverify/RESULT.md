# 复测: review 修复后的层, 真机重跑 (2026-09-23 06:24–06:30)

commit 9d54aaa 之后又改了一处 (启动方式), 最终复测对应本目录 reverify.txt。

## 判据: 与 review 前那次通过的运行逐项相等

| | 层自报 frames | 层自报 render_pass | refbench 自报 frames_submitted | clean_exit |
|---|---|---|---|---|
| review 前 (已验) | 3600 | 32400 | 3600 | true |
| review 后 (本次) | 3600 | 32400 | 3600 | true |

交叉校验仍然成立 (层的计数 = 被测应用自己的账), 9 pass/帧不变。
层里新加的空指针失败分支**一条都没触发** (logcat 里没有对应的 RK_LOG_ONCE 输出)。

## 新增缓解生效

`enable_layer.sh probe` 自动启动目标, 检测到层加载成功后立刻清掉全局属性:

    层已加载, 全局属性已清 (属性暴露窗口约 4 秒)

即 `debug.vulkan.layers` 的误伤窗口从"整个测试时长"压到 **4 秒**。

## 复测捡到的一个新问题: 别用 monkey 启动被测应用

第一次复测拿到 frames_submitted=3339 / clean_exit=false, 差点当成层的回归。
**无层对照**立刻否掉了这个猜测 —— 无层同样跑不满。根因是我在脚本里图省事用了
`monkey -p <pkg> -c LAUNCHER 1` 启动:

| 启动方式 (均为**无层**对照) | frames_submitted | clean_exit |
|---|---|---|
| `am start -n <解析出的 activity>` | 3600 | true |
| `monkey -p <pkg> -c LAUNCHER 1` | 1318 | false |

monkey 会注入事件, 连一个 layer 都没挂就能把 refbench 的运行搅坏。用它启动等于给每次
测量掺进一个与被测对象无关的扰动源。已改回 `am start -n`, 并把解析 activity 的逻辑
写进 `launch_pkg`。

同期还遇到一次 USB 掉线 (adb `device not found`, 约 100 秒后自行恢复), 也会把
refbench 的运行截断 —— 判断"跑没跑满"时要先排除这两个外因, 别急着归因到层。

**教训与本轮主线一致**: 有/无层对照不是走过场。这一轮它挡掉了两次误判 ——
一次是"只读层把 swapchain 弄成 0×0"(真回归), 一次是"复测跑不满"(假回归)。
