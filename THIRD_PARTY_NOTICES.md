# 第三方组件与移植来源声明 (THIRD_PARTY_NOTICES)

本仓库包含移植自下列上游开源项目的功能实现。上游均为宽松许可，允许使用、修改与再分发，
但要求保留版权与许可声明。**这些声明不是可选项**：任何新增的移植文件都必须在代码头部注明
来源，并在此处登记。

---

## 1. SimRun

- 上游: <https://github.com/unixzii/SimRun>
- 许可: MIT License
- 版权: Copyright (c) 2026 Cyandev
- 引入 commit: `e77beff`
- 本仓库移植的部分:
  - agent skill 分发机制（`phonefarm skill`；对应上游 `simrun skill [install]`）
  - 「脚本 + 结构化观测」两段式（对应上游 `simrun eval` + `script.dylib`）——仅借设计，无代码

## 2. FluidUse

- 上游: <https://github.com/FluidInference/FluidUse>
- 许可: Apache License 2.0
- 版权: Copyright FluidInference
- 引入 commit: `9384e70`
- 本仓库移植的部分:
  - 资产可复现管线（pinned manifest + SHA-256 校验）——工程模式，无代码
  - 本地决策模型运行时的接入口设计（`docs/UPSTREAM_PORT_ANALYSIS.md` §5.2）——尚未落地

## 3. iPhone-use

- 上游: <https://github.com/zhongerxin/iPhone-use>
- 许可: MIT License
- 版权: Copyright (c) 2026 ZHONG XIN
- 引入 commit: `08ed5b0`
- 本仓库移植的部分:
  - **观察预算契约**（`docs/SPEC_OBSERVATION_BUDGET.md`）——对应上游 `server/wda_client.py`
    的紧凑控件树 / 去重 / 有界滚动 / 截图复用
  - **组合动作原语**（`src/collect.rs`）——对应上游 `pua_scroll_find` 与 `pua_collect_list`
  - **MCP `structuredContent`**（`src/serve.rs`）——对应上游 `structuredContent` 返回约定
  - 上游自身的依赖（Appium / WebDriverAgent / appium-ios-device）与本仓库无关，未引入

---

# 上游许可全文

## MIT License (SimRun)

```
MIT License

Copyright (c) 2026 Cyandev

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## MIT License (iPhone-use)

```
MIT License

Copyright (c) 2026 ZHONG XIN

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Apache License 2.0 (FluidUse)

FluidUse is licensed under the Apache License, Version 2.0. 全文见
<https://www.apache.org/licenses/LICENSE-2.0>。本仓库未复制 FluidUse 的源代码，
若后续引入，须随附完整许可全文与 NOTICE 文件（如有）。
