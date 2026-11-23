# Bamboo Music Player

> **AI 生成提示**
>
> 本仓库的代码、文档与测试主要由 AI 编码助手生成或在其协助下编写，可能包含
> 错误、遗漏或未经验证的实现。请在使用前自行审查与测试；作者不对因使用本项目
> 造成的任何损失负责。

Windows 原生音乐播放器。主窗口使用 Win32 + GDI 自绘，Direct2D 只用于桌面歌词 /
迷你播放器等覆盖层；业务核心与 UI 完全分离（`Snapshot` / `Command`）。

## 快速开始

| 用途 | 命令 |
| --- | --- |
| 构建 / 运行 | `cargo run` |
| 发布构建 | `cargo build --release` → `target\release\bamboo-music-player.exe` |
| 窗口与绘制自检 | `cargo run -- --self-test` |
| 性能基准（1 万首合成队列） | `cargo run -- --bench 5` |
| 启动时加载文件 / 目录 | `cargo run -- "F:\music"`（也支持拖放） |
| 打开曲库页并索引 | `cargo run -- --page library --library "F:\music"` |

测试辅助参数（只影响本次运行，不写回配置）：`--page <now_playing|queue|library|folders|search|discover|smart|duplicates|stats|downloads|settings>`、
`--sort <title|artist|album|added|duration|plays|played|path>`、`--folder <dir>`、
`--settings-tab 0..14`、`--theme <dark|light|system>`。

## 子模块

原生 UI 框架在独立仓库
[`bamboo-rust-native-ui`](https://github.com/wifi-left/bamboo-rust-native-ui)，
以子模块 `native-ui/` 挂在本仓库下。首次克隆请用：

```powershell
git clone --recursive https://github.com/wifi-left/bamboo-music-player.git
```

已有克隆更新后执行 `git submodule update --init --recursive`，否则
`native-ui/` 为空，引用该框架的构建步骤会失败。

## 功能

### 播放

- 播放 / 暂停、上一首 / 下一首、拖动或键盘 seek、音量 / 静音、随机 / 循环；
  会话恢复（只恢复队列位置，曲内从 0:00 开始）、退出与周期自动保存。
- 队列操作：右键上移 / 下移 / 置顶 / 置底；按歌名 / 歌手 / 专辑 / 来源即时
  筛选；乱序时按随机顺序显示整张队列，进入页面自动定位当前曲目。
- 队列页和正在播放页副标题显示当前播放位置“第 12 / 245 首”，筛选时同时显示
  匹配数量。
- 随机顺序用 track key + cursor 持久化；`.mp3list` 重开时保留原顺序与进度，
  新扫描到的文件追加到末尾，播完自动重新洗牌。
- 托盘：显示 / 隐藏、播放控制、音量、随机 / 循环、桌面歌词开关、退出。

### 曲库与文件夹

- SQLite 索引 + 目录监听；原生搜索框、排序菜单、整页分页列表；虚拟列表只绘制
  可见行。
- 添加 / 移除目录、重新扫描（扫描中同一按钮变为“取消索引”）、全部入队、检查
  缺失文件；行右键可从索引移除曲目（不删除文件）。
- 文件夹页按需读取单个目录（后台线程），支持后退 / 前进 / 上一级 / 刷新 /
  播放全部 / 加入队列；地址栏可直接输入路径，Esc 还原；行右键可播放 / 导入 /
  加入曲库 / 复制路径 / 在资源管理器中打开。
- 封面：当前曲目封面用于播放条 / 卡片 / 独立歌词 / 桌面歌词 / 迷你播放器；
  列表行显示 64px 缩略图。大图用 WIC 在解码期裁剪缩放，失败再回退 `image`。

### 歌词与覆盖层

- 正在播放页：歌曲卡片、可滚动 / 可点击歌词子窗体、逐字 KTV、大歌词、独立
  歌词窗口、歌词编辑器（原生 EDIT + Ctrl+S）和分享图。
- 独立歌词窗口：窗口模式使用原生标题栏，F11 切换无边框全屏；记住位置、大小、
  显示模式和最大化状态。
- 桌面歌词 / 迷你播放器：原生分层窗口，可锁定 / 点击穿透 / 置顶，独立行数、
  字号、对齐、尺寸、帧率（15–120 fps）；背景支持纯色 / 渐变 / 图片 / 模糊封面，
  并可自定义字体、圆角、阴影、描边、透明度。
- 桌面歌词底部信息可切换为“歌名 - 歌手”或“歌手 - 歌名”。

### 设置

曲库 / 播放 / 歌词 / 独立歌词 / 桌面歌词 / 迷你 / 窗口 / 外观 / 性能 / 系统 /
网络 / 网页控制 / 存储 / 快捷键 / 调试。内容超出时可用滚轮或右侧滚动条查看。

- 播放：无缝播放、交叉淡化、ReplayGain（含扫描）、10 段均衡器、睡眠定时。
- 歌词：字号、行距、对齐、翻译、逐字填充、颜色与面板背景；所有数值项都可点击
  直接输入。
- 外观：暗 / 亮 / 跟随系统、强调色、字体、列表密度、帧率上限（15–120 fps）、
  纯色 / 模糊封面 / 自定义图片背景。
- 窗口与系统：关闭行为、启动最小化、记住位置、录屏与歌词捕获保护、单实例、
  开机启动、文件关联、SMTC、通知、跳转列表、设置 JSON 导入 / 导出 / 恢复默认。

### 网页控制与系统集成

- 网页控制默认关闭。设置 → 网页控制 可开启本机 / 局域网浏览器控制面板，配置
  监听地址、端口、访问密钥与只读模式；面板提供播放控制、进度、音量、随机 /
  循环、队列点播，并实时显示封面、当前与下一句歌词。
- 仅监听 `127.0.0.1` 时免密钥；绑定 `0.0.0.0` 必须设置密钥。服务校验 `Host`
  与 `Origin`，只读模式拒绝所有控制命令。
- 单实例守卫 + 第二实例命令行转发、SMTC、切歌通知、任务栏跳转列表、录屏捕获
  保护；便携模式不写注册表与系统跳转列表。

### 界面与输入

- 主窗口全部 GDI 自绘，持久双缓冲 + 脏包围盒 blit；动画只失效变化区域，暂停 /
  最小化 / 页面静止时停止计时器，无周期重绘。
- 复杂控件使用原生子窗体：曲库排序是 owner-draw LISTBOX，滚动歌词是独立 GDI
  子窗体，输入使用透明背景的原生 `EDIT`。
- 键盘全局热键可自定义（默认 `Ctrl+Alt+P` 播放暂停、`Ctrl+Alt+←/→` 切歌、
  `Ctrl+Alt+M` 桌面歌词、`Ctrl+Alt+Q` 显示 / 隐藏），带格式校验、重复绑定检测
  与占用提示；Esc 可清空绑定。界面快捷键只在本窗口拥有前台焦点、且焦点不在
  原生输入框时处理。
- 手柄全局快捷键可自定义组合（如 `LB+RB`、`Start+A`），默认保留
  A/B/X/Y/Start/D-Pad 单键映射；其他程序全屏（游戏）时单键绑定自动忽略，
  组合键仍然生效。“手柄全局快捷键”和“手柄无障碍导航”是两个独立开关。
- 手柄无障碍导航（仅本播放器为前台窗口时生效）：D-Pad / 左摇杆移动焦点，
  A 确认，B 返回，LB/RB 切换焦点，Start 播放 / 暂停；设置页 LT/RT 或键盘
  PageUp/PageDown 切换设置分类。
- 焦点顺序按界面布局：侧栏自上而下，页面内容自上而下 / 从左到右，播放条最后；
  设置项到达首 / 尾后继续移到相邻区域。设置 → 曲库的开关、查重方式、扩展名、
  添加目录和每个目录的“移除”都可获得焦点。
- 播放条音量 / 进度采用两级操作：焦点移上去时方向键仍可移走；按 Enter/Space/
  手柄 A 进入编辑模式（实线强调框）后方向键才调整音量 / 快进，
  B/Esc/Backspace 退出。鼠标拖动不受影响。
- 搜索框、队列筛选、文件夹地址栏、扩展名输入框和设置文本行都在焦点环内。
  手柄 A 确认后弹出 Windows 手柄键盘（`CoreInputView` Gamepad 布局，
  Windows 11 24H2+；旧系统回退触控键盘 / osk.exe）；A 重新打开被关掉的键盘，
  B/Esc 取消输入并回到焦点环。
- 主窗口、独立歌词、桌面歌词、迷你播放器都提供 UI Automation 片段树，焦点变化
  通过 `UIA_AutomationFocusChangedEventId` 上报。

### 性能（历史测量，供参考）

debug 构建、175% DPI、Intel 核显：

- 暂停：GPU 0.00%。
- 播放 + 滚动歌词：GPU < 1%，UI 单核 CPU ≈ 4.3%。
- 桌面歌词 + 迷你播放器暂停：CPU / GPU 均 0%（事件唤醒，定时器完全停止）。
- `--bench 3`：私有约 28MB、工作集约 50MB、平均绘制约 0.61ms/帧（10k 队列，
  只 blit 脏包围盒）。

## 测试与诊断

- `cargo test`：273 passed / 0 failed / 6 ignored。
- `--self-test`：创建窗口、绘制一帧后退出，退出码 0 表示通过。
- `--bench [秒]`：输出 tick / 帧数、平均绘制耗时与私有内存。
- 待办与验收矩阵见 [`HANDOFF.md`](HANDOFF.md)。

## 项目结构

- `src/core/`：UI 无关核心（audio / library / lyrics / config / playlist / api /
  download / radio / util / platform / overlays / web），不引用 egui/eframe/winit。
- `src/`：Win32 原生外壳（`main.rs` 消息循环、`app.rs` 界面状态与输入、
  `paint.rs` GDI 绘制、`lyrics_view.rs` / `lyrics_window.rs` 歌词子窗体）。
- `ref/archived/bamboo-egui/`：0.1.0 egui 版只读归档，不参与构建。
- 方案与踩坑：[`docs/WIN32_UI_PLAN.md`](docs/WIN32_UI_PLAN.md)、
  [`docs/NATIVE_UI_EXPERIENCE.md`](docs/NATIVE_UI_EXPERIENCE.md)。

## 许可

Bamboo Music Player Non-Commercial License 1.0，详见根目录 [LICENSE](LICENSE)。
禁止商业使用，分发 / 修改需署名原作者。
