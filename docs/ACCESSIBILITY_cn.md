# 无障碍

TUI 运行于终端，因此平台自身的无障碍栈（屏幕阅读器、放大镜、
终端级主题）承担了大部分工作。TUI 提供一小组开关，为屏幕阅读器和低动效
用户降低视觉运动与密度。

## 快速参考

| 开关 | 默认值 | 效果 |
| --- | --- | --- |
| `NO_ANIMATIONS=1` 环境变量 | 未设置 | 启动时强制 `low_motion = true` 且 `fancy_animations = false`。会覆盖 `settings.toml` 中已保存的设置。 |
| `low_motion` 设置 | `false` | 采用更平缓的流式节奏和更低的刷新频率，让光标/状态的运动不那么频繁。页脚水纹条由 `fancy_animations` 单独控制。 |
| `fancy_animations` 设置 | `true` | 页脚喷水条与脉动的子代理计数器。设为 `false` 可让实时回合的界面装饰保持静止。 |
| `status_indicator` 设置 | `dots` | 顶栏状态徽标。`dots` 是紧凑的点循环样式；设为 `off` 将其隐藏。 |
| `calm_mode` 设置 | `false` | 默认折叠工具输出细节并精简状态消息。适合会播报每次重绘的屏幕阅读器。 |
| `show_thinking` 设置 | `true` | 设为 `false` 可完全隐藏模型 `reasoning_content` 块。 |
| `show_tool_details` 设置 | `true` | 设为 `false` 可将工具调用渲染为单行，不展开载荷。 |

## 标准环境变量接口

在你的 shell profile 中设置以下变量，使其对每个会话生效：

```bash
# Force low-motion + no fancy animations.
export NO_ANIMATIONS=1

# Optional: respect the wider terminal-color convention.
export NO_COLOR=1            # honored by the underlying ratatui backend
```

`NO_ANIMATIONS` 接受 `1`、`true`、`yes` 或 `on` 中的任意一个（不区分
大小写）。其他任何值（包括 `0`、`false`、空值或未设置）都不会改动已保存
的设置。

该覆盖在启动时应用一次。会话中途更改该环境变量不会生效——设置只会在下次
启动时重新读取。

## 通过 `/settings` 配置

同样的开关也可以通过命令面板访问：

* `/settings set low_motion on`
* `/settings set fancy_animations off`
* `/settings set calm_mode on`
* `/settings set status_indicator off`

以这种方式写入的设置会持久化到与配置文件同目录的 settings 文件：
默认为 `~/.config/deepseek/settings.toml`（当 `CODESMITH_CONFIG_PATH`
指向自定义 `config.toml` 时，`settings.toml` 位于同一目录）。
如果设置了 `NO_ANIMATIONS` 环境变量，它在启动时仍然优先，因此只有取消该
环境变量才能让你保存的选择生效。

Tilix 和 Terminator 会话会自动以低动效模式启动，因为这些基于 VTE 的终端
在活跃回合期间曾报告过明显的重绘闪烁。如果你的终端版本渲染正常，也可以
在启动后覆盖已保存的设置。

## 面向屏幕阅读器用户的说明

* `low_motion` 将空闲重绘循环放慢到约每帧 120ms，使光标不会被不断重新
  定位。配合 `calm_mode`，重绘率足够低，VoiceOver / Orca 的播报可以随
  模型输出线性推进，而不是每次刷新都重读整个屏幕。
* transcript 是纯文本——没有图像或 canvas 渲染——因此任何与平台无障碍
  服务集成的终端（如 macOS Terminal.app、iTerm2、Ghostty、Windows
  Terminal）都会将渲染内容直接传递出去。
* 如果你发现某个 UI 表面在 `low_motion = true` 时仍然产生动效，请携带
  截图或终端录屏，记录到 [ISSUES.md](ISSUES.md)（issue 台账）中。

## 相关历史

* 本页面、现有标志的文档以及 `NO_ANIMATIONS` 启动覆盖层是作为同一次
  无障碍改造落地的。
* 页脚状态栏现在使用当前主题的对比色对，而不是一套专用调色板。
