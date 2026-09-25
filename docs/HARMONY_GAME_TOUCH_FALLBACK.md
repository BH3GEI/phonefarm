# HarmonyOS 游戏触控输入备用方案

> 本文记录已在一台无 root 的 HarmonyOS 真机上验证的备用输入路径。优先目标仍是由设备识别的原生手柄输入；这里的触控坐标必须按游戏、分辨率和 UI 布局校准，不能当作通用手柄后端。

## 真机记录（2026-09-24）

| 项目 | 结果 |
| --- | --- |
| 设备 | 华为 nova 16，HarmonyOS 6.1.0.130，API 24 |
| 权限 | `hdc shell` 为普通 `shell` 用户；没有 root |
| 目标 | 鸿蒙原生《原神》`com.miHoYo.Yuanshen`，开放世界，横屏 2520 × 1152 |
| 输入 | 通过 `hdc shell` 执行系统自带的 `uinput -T -m`，同时移动两根手指 |
| 观察 | 左侧虚拟摇杆推动角色，右侧拖动旋转视角；命令返回 `fingerCount:2`，操作前后截图可见角色位置与视角均变化；同一命令经 `phonefarm script` 运行后生成的 `step1.jpg`、`step2.jpg` 也显示了视角变化 |
| 范围 | 仅验证一次短手势；未验证长跑、不同分辨率、所有游戏或原生手柄识别 |

实测命令（坐标只适用于上述画面）：

```sh
hdc -t <connect-key> shell 'uinput -T -m 400 850 400 700 1300 650 1450 650 -k 300 500'
```

`-T -m` 的两组起点和终点分别对应两根手指；`-k 300` 在到达终点后保持 300 ms，最后的 `500` 表示移动用时 500 ms。OpenHarmony 的 [uinput 文档](https://github.com/openharmony/docs/blob/master/zh-cn/application-dev/dfx/uinput.md)说明该命令最多支持三指同时移动。

## 放入 phonefarm 的方式

[`examples/harmony_genshin_multitouch_smoke.json`](../examples/harmony_genshin_multitouch_smoke.json)使用现有 `script` 模式的 `shell` 步骤，无需改动 `Device` 核心。先手动将原生《原神》置于开放世界，再执行：

```sh
export PATH="/Applications/DevEco-Studio.app/Contents/sdk/default/openharmony/toolchains:$PATH"
command -v hdc
hdc list targets
(cd src && cargo build --release)
codesign --force --sign - src/target/release/phonefarm
src/target/release/phonefarm script \
  --task harmony_genshin_multitouch_smoke \
  --serial hdc:<connect-key> \
  --settle-ms 300 \
  examples/harmony_genshin_multitouch_smoke.json
```

上面的 HDC 路径是本机 DevEco Studio 的安装位置；其他机器应换成实际 SDK 的 `toolchains` 目录。脚本的第一步截图是动作前状态，第二步截图是动作后状态。应人工核对两张图中角色位置与视角；当前 `shell` 步骤不把命令失败自动判成整局失败，控制台还会截短命令输出。实测当 `hdc` 不在 `PATH` 时，脚本仍以 `achieved=true` 结束，却不生成截图。因此至少要确认两张截图存在且画面有预期变化，不能只看脚本的结束状态。若要查看完整 `fingerCount:2` 输出，应单独执行前面的 HDC 命令。请不要将这个坐标样例直接用于重复压测：角色位置会逐轮变化，轨迹不再一致。

## 边界与后续

- `uinput` 是系统提供的命令行注入工具；它与 Linux `/dev/uinput` 虚拟设备接口不是一回事。该真机的普通 `shell` 用户无法访问 `/dev/uinput` 或 `/dev/uhid`。
- `uinput` 当前帮助列出键盘、鼠标、触摸、触控笔和触控板，没有游戏手柄设备创建或摇杆轴事件选项。触控测试成功不等于原生手柄注入成功。
- HDC 试探性发送手柄 B 键码（`uinput -K -d 2302 -u 2302`）后，游戏设置页没有可见响应，`hidumper -s MultimodalInput -a -l` 仍列出原来的八个输入设备，没有虚拟手柄。原神“控制设备”菜单当时只显示“触屏”。由于现场没有实体手柄，这不能判定游戏对实体手柄的支持情况；它只能说明单独注入按键码尚未形成可用的手柄控制链路。
- OpenHarmony 输入服务源码包含 `AddVirtualInputDevice` 和摇杆事件路径，且源码里的系统应用检查允许 `TOKEN_SHELL`。这是进一步研究 HDC 软件手柄的线索，不等于华为商用镜像已开放可调用接口；DevEco Studio 公共 NDK 的 `oh_input_manager.h` 也没有虚拟手柄创建设备接口。尝试从 HDC shell 执行上传到 `/data/local/tmp`、已设为可执行的原生探针时，设备返回 `Permission denied`，因此本轮连私有库是否可加载也未能确认。目前尚未在该设备创建可被游戏识别的虚拟手柄。
- 当前原神插件声明 `GamepadOnly`，而 HDC 后端的 `gamepad_*` 方法返回“不支持虚拟手柄注入”。本文件只保留可用的备用路径，不改变插件对手柄能力的声明。
- 若以后采用触控作为稳定评测输入，应给每个游戏单独保存坐标配置、分辨率、起点状态和复位步骤，再检验每轮轨迹及画面是否一致。
