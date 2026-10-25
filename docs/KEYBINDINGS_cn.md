# 键盘与交互参考

REPL 在提示符处与回合运行中绑定的全部按键。`/help` 列出命令，本文件列出按键。
空提示符下按 `?` 会在终端里打印同一张表，`/keybindings` 也可以重新打印。

涉及两层：

- **提示符处**：行编辑器是 [`rustyline`](https://github.com/kkawakam/rustyline)，
  使用它的 Emacs 键表（或 vi 键表 —— `/config editor_mode vim`）。下文的编辑与
  历史表绝大多数原样来自它。
- **回合运行中**：一个小的字节解码器接管 stdin（仅关闭行缓冲与回显，`Ctrl+C`
  仍以中断信号到达），并喂给一个 composer：模型工作期间你可以继续输入、编辑、
  排队。

## 回合运行中（composer）

| 按键 | 效果 |
| :- | :- |
| *任意文本* | 输入进 composer，状态行下方出现输入行 |
| `Enter` | 排队该消息 —— 它等待的工具调用一结束就发送；若回合先结束，则作为下一轮发送 |
| `Ctrl+J` | composer 内换行（`Enter` 是排队，所以多行要用这个） |
| `Ctrl+X Ctrl+S` | 把已排队内容（含 composer）立即发送给正在运行的回合 |
| `Ctrl+Enter` | 同上「立即发送」，仅在把修饰键随 Enter 上报的终端可用（kitty/CSI-u）；`Ctrl+X Ctrl+S` 到处可用 |
| `Esc` | 若正在跑 shell/子代理则转后台并发送已排队内容；否则中断回合，队列在下一轮发出 |
| `Ctrl+B` | 把正在运行的 shell 转到后台 |
| `Ctrl+X Ctrl+K` | 停掉全部后台工作（3 秒内按两次） |
| `Ctrl+O` | 切换工具调用详情的折叠/展开 |
| `Ctrl+T` | 切换模型的任务清单 |
| `Shift+Tab` | 循环运行模式（从下一轮生效） |
| `Up` | composer 为空时，取回最新一条已排队消息继续编辑 |
| `Down · Tab · 补全` | 回合中不可用（没有历史、没有补全） |
| `Ctrl+C` | 中断回合；按两次退出 |
| `Ctrl+A/E`、`Ctrl+K/U/W`、`←/→`、`Home/End`、`Backspace`、`Delete` | composer 内的编辑（回合中的 kill 键没有 kill ring） |

回合结束时 composer 里剩下的内容会回到下一个提示符 —— 先结束的回合不会吞掉
你正在输入的东西。

## 提示符处

### 通用

| 按键 | 效果 |
| :- | :- |
| `Enter` | 提交 |
| `Ctrl+C` | 清空输入（草稿保留 —— `Up` 可取回）；紧接着再按一次（中间不输入其他内容）退出 |
| `Ctrl+D` | 行内有文本：删除光标后的字符。空行：800ms 内再按一次退出 |
| `Ctrl+S` | 暂存草稿。在空提示符再按一次恢复 |
| `Ctrl+O` | 切换上一轮的工具调用详情折叠/展开 |
| `Ctrl+T` | 切换提示符上方的任务清单面板 |
| `Ctrl+G` / `Ctrl+X Ctrl+E` | 用 `$VISUAL`/`$EDITOR` 编辑草稿，保存后带回 |
| `Ctrl+X Ctrl+K` | 停掉全部后台工作（3 秒内按两次） |
| `Alt+P`（`⌥P`） | 在本会话模型与配置的默认模型之间切换 —— `/model <id>` 可选任意模型 |
| `Alt+T`（`⌥T`） | 切换 extended thinking（off ↔ high） |
| `Shift+Tab` | 循环运行模式：`agent → plan → yolo`（启用对应 feature 时还有 `coordinator`）。不打印任何东西 —— 状态行的 `🚥` 段原地翻转，草稿的文字与光标保持原样 |
| `Esc` | vi 插入模式下：退出插入模式 |
| `Esc`, `Esc` | 行内有文本：清空草稿并保留给 `Up`。空提示符：无操作（rewind 菜单未实现；文件用 `/rewind`，对话用 `/undo`） |
| `?` | 空提示符下：打印快捷键面板 |
| `Ctrl+L` | 重绘屏幕（rustyline） |
| `Ctrl+Z` | 挂起到 shell，`fg` 返回（rustyline） |
| `Ctrl+R` | 反向历史搜索（rustyline） |

`Alt`/`Option` 组合键（`Alt+P`、`Alt+T`、`Alt+B/F/D`、`Alt+Y`）要求终端把
Option 作为 Meta 发送；macOS 上这是终端设置 —— 见 `/terminal-setup`。

### 编辑（readline 惯例，来自 rustyline）

| 按键 | 效果 |
| :- | :- |
| `Ctrl+A` / `Ctrl+E` | 行首 / 行尾 |
| `Ctrl+K` / `Ctrl+U` | 删到行尾 / 删到行首 |
| `Ctrl+W` | 删回到上一个空白 —— 一次删掉整条路径 |
| `Backspace` / `Ctrl+H` | 删除光标前字符 |
| `Ctrl+Y` / `Alt+Y` | 粘贴上次删除的文本 / 轮换 kill ring |
| `Alt+B` / `Alt+F` | 前后移动一个词 |
| `Alt+D` | 删到词尾 |
| `Ctrl+_` | 撤销上一次编辑 |
| `Ctrl+T`（emacs 转置） | 已被占用：`Ctrl+T` 是任务清单 |

### 多行输入

| 方式 | 适用终端 |
| :- | :- |
| 行尾 `\` 后按 `Enter` | 所有终端。`\` 只是标记，发送前会移除 |
| `Ctrl+J` | 所有终端 |
| `Shift+Enter` / `Alt+Enter` | 仅在把修饰键随 Enter 上报的终端 |
| 粘贴多行文本 | 所有终端（括号粘贴）；换行会保留 |

### 历史

| 按键 | 效果 |
| :- | :- |
| `Up` / `Down`、`Ctrl+P` / `Ctrl+N` | 浏览历史；空提示符下 `Up` 先取回被清空的草稿，再取回排队消息，最后才是历史 |
| `Ctrl+R` | 反向搜索；`Enter` 接受并发送，`Tab`/`Esc` 接受并继续编辑，`Ctrl+C` 取消 |

历史按**工作区**存放在 `~/.codesmith/history/<workspace>`，上限由
`max_input_history` 决定（默认 500）。升级前那份单一历史文件
`~/.codesmith/history` 会在某个工作区还没有自己的文件时读取一次，并保留原地。

### 补全

| 按键 | 效果 |
| :- | :- |
| `Tab` | 补全光标处的词；再按一次列出候选 |
| `Tab` 于 `/` | 命令名（提示符开头**以及**句中空格之后） |
| `Tab` 于 `@` | 工作区路径，来自缓存索引（尊重 `.gitignore`、跳过隐藏文件、最多每 10 秒重建一次、受 `mention_walk_depth` 与 `mention_menu_limit` 限制） |
| `Tab` 于参数 | 命令声明的封闭词表（`/mode agent…`、`/effort high…`、`/mcp list…`） |
| `Tab` 于 `!` 行 | 含 `/` 的词补全工作区路径；否则补全本项目自己的 `!` 历史（最新优先、去重） |

### `!` shell 模式

以 `!` 开头的行会在你自己的 shell 里、于工作区内执行，打印输出，并把命令与
输出作为**一条**消息交给模型，让该轮据此回答。它不需要审批：审批面用于把关
模型运行什么，而不是把关你输入什么。设置
`respond_to_shell_commands = false` 可以只记录运行、不启动回合。

已知限制：输出在命令结束后才捕获并打印（不流式）、两个流合并、尾部截断在
20000 字符。

## vim 模式

`/config editor_mode vim`（或 settings.toml 里 `composer_vim_mode = "vim"`）
把提示符切换到 rustyline 的 vi 键表：插入/命令模式、动作
（`h j k l w b e 0 $ ^ f F t T ; ,`）、操作符（`d c y` 配动作与双写形式）、
文本对象（`iw aw i" a" i( a[ a{`）、`x r s S ~`、`p P`、`>> <<`、`J`、`u`、
`.`，以及 `v`/`V` 可视模式。`/config editor_mode normal` 回到 Emacs 键，
切换在下一个提示符生效。

未实现，并写明原因：`vimInsertModeRemaps`（Claude Code 的 `jj` → `Escape`）。
rustyline 14 没有向按键处理器暴露任何可用于退出 vi 插入模式的命令，处理器无法
合成 `Escape`。

## 终端需要具备什么

| 功能 | 要求 |
| :- | :- |
| `Shift+Tab` | 终端需发送 `ESC [ Z`（Terminal.app、iTerm2、tmux 默认如此）；kitty 协议的 `CSI 1;2Z` 不被 rustyline 14 解析 |
| `Alt`/`Option` 组合键 | 终端里把 Option 设为 Meta（`/terminal-setup` 会打印各终端的设置项） |
| `Ctrl+Enter` | 把修饰键随 Enter 上报的终端；通用写法是 `Ctrl+X Ctrl+S` |
| `Shift+Enter` | 同上；`Ctrl+J` 与行尾 `\` 到处可用 |
| composer、回合中按键 | 需要真实 TTY。管道输入（`codesmith < file`）按设计让回合中的按键捕获保持惰性 |
| `/copy`、粘贴图片 | `PATH` 上有剪贴板工具（`pbcopy`、`wl-copy`、`xclip`、`clip`）；找不到时命令会指明要装什么 |

## 这里没有绑定

- 块状可视模式（Claude Code vim 模式里的 `Ctrl+V`）。
- 键位配置文件：绑定是编译进去的，`/keybindings` 是只读表（唯一的键盘设置是
  `/config editor_mode`）。
- 全屏渲染专属交互（transcript viewer 的 `{`/`}`、鼠标支持、`/scroll-speed`、
  `/focus`）。
- 语音听写。