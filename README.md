# phonefarm

> 本项目基于 [BH3GEI/phonefarm](https://github.com/BH3GEI/phonefarm) 开源工程协同演化，由 HGamey 组织承载面向多平台渲染能效的专用特性开发。

移动端设备自动化与性能评测框架：设备连接（Android adb / OpenHarmony hdc / iOS WebDriverAgent）、施加系统参数与 Vulkan 层改动（`knobs/`）、测量（sysfs + ftrace / HiSmartPerf）、统计判定与现场还原。上层决策由 [game_opt_loop](https://github.com/HGamey/game_opt_loop) 负责。

## 构建

```bash
cp secrets.env.example secrets.env     # 只有 VLM 通路需要密钥
cd src && cargo build --release && cp target/release/phonefarm .. && cd ..
codesign --force --sign - ./phonefarm  # Apple Silicon 必须重新签名，否则会卡在 _dyld_start
./phonefarm devices                    # 确认设备在线
```

`adb` 查找顺序：`ADB_BIN` → 仓库根 `platform-tools/` → `PATH` → 常见 SDK 目录。

## 文档

- 完整说明、各通路用法：[HGamey/docs · repos/phonefarm](https://github.com/HGamey/docs/tree/main/repos/phonefarm)
- 设计与各 SPEC：[repos/phonefarm/docs](https://github.com/HGamey/docs/tree/main/repos/phonefarm/docs)
- 性能优化回路 loop_v1：[repos/phonefarm/loop_v1/README.md](https://github.com/HGamey/docs/blob/main/repos/phonefarm/loop_v1/README.md)
- 给 AI 编程助手：[AGENTS.md](AGENTS.md)、[skills/phonefarm/SKILL.md](skills/phonefarm/SKILL.md)

本仓库只放代码，规则见 [AGENTS.md](AGENTS.md) 末尾。
