# AGENTS.md

Bamboo Music Player — Windows 原生音乐播放器。主窗口用 Win32 + GDI 自绘，
Direct2D 只用于覆盖窗口。业务核心在 `src/core/`，原生外壳在 `src/`。

## 渲染与局部重绘

性能来自**只重绘变化的区域**，不是靠重构或降帧。

- 状态变化只 `InvalidateRect(hwnd, &changed_rect, FALSE)`；`WM_PAINT` 只绘制
  `ps.rcPaint` / `GetUpdateRect` 覆盖的区域。
- 大状态变化允许一次整体更新：页面切换、主题 / DPI / 窗口尺寸变化、列表批量
  重建等，直接整窗失效重绘，避免碎片化失效和多次 `WM_PAINT`。
- 列表滚动只绘制新露出的行和滚动条。
- 进度条、歌词当前行、时间文本各自维护矩形；只有变化的矩形失效；无动画时停掉
  `SetTimer`。
- `IDWriteTextLayout`、封面位图、字体 / 主题按版本缓存；禁止每帧重建或重复解码。
- 禁止 `InvalidateRect(hwnd, None, ...)` 式整窗刷新；禁止每帧全量 `BeginDraw` +
  全屏绘制。
- 不要在失去焦点时停止动画（失焦 ≠ 不可见）。按 z-order 计算可见矩形，动画脏区
  与可见区求交，完全遮挡才停止失效 / 发布；重新可见时整窗重绘。
- 计算遮挡时**不得把 `WS_EX_LAYERED` / `WS_EX_TRANSPARENT` 窗口当作不透明
  遮挡物**（桌面歌词、迷你播放器、tooltip 都可能部分透明），否则透明区域下方的
  主窗口不会重绘。宁可多画一点这些窗口下方的脏区，也不要跳过。

## 窗口与子窗体

- 主窗口全部 GDI 自绘。可滚动歌词是独立子窗体（`WS_CHILD`，GDI 双缓冲），自己
  处理滚动 / 悬停 / 点击 / 滚动条 / KTV 填充；父窗口只画歌曲卡片与面板外壳。
- 独立歌词窗口（`src/lyrics_window.rs` + `src/lyrics_screen.rs`）：窗口模式用
  原生标题栏 / 边框，保留系统最小化 / 最大化 / 关闭与主题色（不强制改 DWM 配色）；
  F11 切换无边框全屏。窗口壳用独立 `Painter` 自绘，歌词仍是同一个 `lyrics_view`
  子窗体；浮动模式下子窗体必须按自己在父窗口中的偏移重画同一套装饰背景椭圆，
  否则歌词区域会出现与背景不连续的纯色块。窗口无 owner 且 `WS_EX_APPWINDOW`，
  主窗口最小化 / 隐藏到托盘时仍可显示。
- 播放设置弹窗是独立子窗体：`SetWindowPos(HWND_TOP)` 置于歌词子窗体之上；重叠的
  自绘子窗体必须带 `WS_CLIPSIBLINGS`，否则失效重绘会越过兄弟窗口，表现为
  “弹窗被歌词条带盖住”。
- 主窗口类不使用 `CS_DBLCLKS`：每次按下都当普通单击处理，快速连点控制按钮不会被
  系统双击吞掉。
- 输入框优先“透明背景、无边框 `EDIT` + 自绘背景”，不要自绘文本编辑器。
- 复杂 / 可滚动 / 有选择的控件优先用原生子窗体（类似 MFC），子窗体自己处理
  悬停、选择、滚动与擦除；主窗口只绘制静态外壳。
- GDI 绘制必须走 `Painter` 的持久双缓冲，只 blit 脏包围盒；不要直接画到
  `BeginPaint` 的窗口 DC。
- 不要在同一个 GDI 窗口表面上混画 `ID2D1DCRenderTarget`：它会把内部位图 BitBlt
  覆盖整个绑定区域，子矩形绑定还会出现 DWM 不刷新。
- 桌面歌词 / 迷你播放器等跨窗口覆盖面用分层窗口（`WS_CHILD` + `WS_EX_LAYERED`，
  由 `app.manifest` 启用）；D2D DC target 画到 DIB 后用
  `UpdateLayeredWindowIndirect` 发布脏条。小面积覆盖层的
  `ID2D1DCRenderTarget` 使用 `D2D1_RENDER_TARGET_TYPE_SOFTWARE`：硬件 target 会
  为小 DIB 初始化 D3D / 驱动，多占约 30MB，收益为零。
- 独立歌词窗口同样要记住原生最大化状态：保存还原尺寸（`WINDOWPLACEMENT`
  的 `rcNormalPosition`），重新打开时用 `SW_SHOWMAXIMIZED`，不能在最大化时把
  整屏尺寸当成普通窗口尺寸保存。

## 输入、焦点与无障碍

输入分三层，互不越界：

- 全局键盘热键（`RegisterHotKey`）始终生效。
- 界面键盘快捷键（`App::on_key`）只在主窗口 / 独立歌词窗口拥有前台焦点、且焦点
  不在原生 `EDIT` 时处理。
- 手柄分为窗口聚焦时的界面导航与失焦后的全局组合键。

具体规则：

- 手柄界面导航复用 `on_key`（合成方向键 / Tab / Enter / Esc），焦点变化走
  `accessibility::notify_focus`。窗口聚焦时 A/B/LB/RB/Start/D-Pad 归界面，
  包含这些键的全局组合被遮蔽，避免同一次按键触发两个动作。
- 手柄全局快捷键（`shortcuts.gamepad`）与无障碍导航（`shortcuts.gamepad_ui`）
  是两个独立开关，轮询在任一开启时运行。
- 焦点顺序按视觉布局：侧栏自上而下、页面内容自上而下 / 从左到右、播放条最后；
  排序用控件垂直中心而不是 `top`（播放键比相邻按钮高）。页面上没有列表时不得把
  焦点设成不可见的 `Row`（`reveal_gamepad_focus` 用 `page_shows_rows` 判断）。
- 设置页手柄 LT/RT 或键盘 PageUp/PageDown 循环切换设置分类（切分类清空焦点环）；
  设置行到达首 / 尾后用 `move_focus` 继续到相邻目标，不得困在设置页。
- 设置 → 曲库的开关 / 查重 / 扩展名 / 添加目录 / 移除目录都是独立焦点目标
  （`Target::SettingToggle`、`SettingDedupeOption`、`SettingAddFolder`、
  `FolderRemove`、`Input`）。目录行的焦点目标数量必须按“实际目录数 - 滚动偏移”
  限制，并在 `focus_target_visible` 再校验，不能按可用槽位生成幽灵焦点。
- 从侧栏焦点切换页面时保留 `Nav(index)` 焦点（`on_click` 的 nav 分支），否则下一
  次方向输入会落到播放条。
- 有焦点环时方向键在无障碍控件间移动焦点（列表行 / 设置行保留上下移动）；播放条
  音量 / 进度需要 Enter/Space/手柄 A 进入编辑模式（`App::adjusting`，实线强调框）
  后方向键才调整 / 快进，B/Esc/Backspace 退出；无焦点环时保留旧的 ←/→ 快进、
  ↑/↓ 滚动。
- 搜索框、队列筛选、文件夹地址栏、扩展名输入框和设置文本行都进入焦点环
  （`Target::Input`）。手柄按 A 确认文本输入后弹出 Windows 手柄键盘
  （`CoreInputView::TryShowWithKind(Gamepad)`；旧系统回退 `ITipInvocation` 触控
  键盘 / osk.exe）。EDIT 获得焦点后手柄输入交给系统键盘，播放器不再响应同一批
  按键；A 重新打开被关闭的键盘，B/Esc 取消输入并回到焦点环。只有确认后 EDIT
  才获得焦点并接受输入。
- 其他程序全屏（游戏）时只响应多键手柄组合，单键绑定忽略
  （`shortcuts.gamepad_fullscreen_only_chords`）；单键与组合同时匹配时组合优先。
- 手柄组合绑定在“全部按键松开”时提交（记录按住期间的最大组合），Esc 清空；
  捕获期间停止全局键盘热键注册。

## 交互与菜单

- 列表行 / 滚动条命中必须同时校验页面（仅播放队列 / 曲库）；其他页面的空白或
  卡片点击不得落成 `PlayIndex`（曾导致正在播放页点卡片时重播 / 上下一首 / 随机曲）。
- 播放条封面 / 歌名：左键跳转“正在播放”；右键弹当前曲目菜单（复制歌名 / 歌手、
  打开文件位置、打开 URL、复制 Bamboo 歌曲 ID）。菜单使用原生 `TrackPopupMenu`，
  数据必须在 `State` 借用结束后克隆并弹出，避免模态循环重入。
- 歌词列表右键：子窗体上报行号（`WM_APP_LYRIC_MENU`），主窗口弹菜单（在此处播放 /
  复制文本 / 复制时间点 / 重新加载 / 导入 / 导出 / 打开歌词位置）。“打开歌词位置”
  仅本地曲目且存在实际歌词文件时可用；导入 / 导出对话框在菜单返回后执行，不能在
  菜单回调里持有 `State` 借用。
- 队列 / 曲库行右键菜单在 `WM_CONTEXTMENU` 中优先于曲目信息菜单；菜单关闭后再执行
  命令（移除 / 下一首播放 / 加入队列 / 清空）。队列页头部提供导入 / 导出 / 清空
  快捷按钮，导入导出格式为 `.mp3list`（`core::playlist::mp3list`）。

## 列表、队列与会话

- 虚拟列表刷新：后台刷新必须保留已加载行数（避免滚到深处后只重载第一页而空白）；
  搜索 / 排序 / 进页要重置 scroll；任何行数收缩后都要把 scroll clamp 到
  `max_scroll()`。
- 队列筛选必须维护“显示行 → 真实队列下标”的映射；行点击、右键、封面、tooltip、
  当前行高亮都先映射，不能把显示行直接当队列下标。
- 随机顺序 / 进度用 track key + cursor 持久化；`.mp3list` 重开时旧顺序保持不变、
  新文件追加到末尾，不得重新洗牌或丢失当前进度。

## 设置与持久化

- 设置在暂停时也可能变化：离散开关后调用 `AppCore::flush_settings()` 立即落盘；
  连续拖动（音量 / 进度）仍走节流保存，不能每个鼠标移动都写文件。
- 新增设置字段必须带 `serde(default)`，旧 `settings.json` 不能因为缺字段而加载失败。
- 单实例守卫在创建窗口之前执行；第二实例只转发命令行（命名管道）后退出。便携模式
  不写注册表、系统跳转列表，避免污染已安装环境。

## 性能验收

- 空闲（窗口隐藏、无 overlay）：GPU ≈ 0%，CPU < 1%。
- 主窗口可见、播放 + 滚动歌词：核显 GPU < 3%，UI 线程 CPU < 5%。
- 私有工作集目标 < 100MB。
- 空闲（无动画、无输入）时不应有任何周期重绘。
- 优化前先测量（GPU Engine / 进程 CPU / 工作集 / `WM_PAINT` 耗时），用数据判断
  瓶颈；不接受用降帧率、降画质来换占用。

## 硬性约束

- **只维护一套 UI**：根 crate `bamboo-music-player`（Rust 2024 + Win32 +
  Direct2D/DirectWrite）。
- `ref/archived/bamboo-egui/` 是 0.1.0 egui 版的只读参考归档：不得作为依赖、不参与
  根构建、不再同步维护。
- 主窗口使用标准标题栏；不在主窗口重新引入 egui / wgpu / WebView。
- 业务代码（audio / library / lyrics / platform / playlist / api / config）放在
  `src/core/`；新增业务逻辑不得引用 egui/eframe/winit。
- UI 只读 `Snapshot`、只发 `Command`；UI 线程不做 IO / 解码 / 数据库。
- 大列表必须虚拟化，只绘制可见行。
- 大封面优先 WIC（clipper / scaler）解码，失败再回退 `image` crate；禁止在 UI 线程
  解码，或在切歌路径上重复解码同一张大图。

## Git

- 后续修改直接提交并 push 到 `main`（用户已授权，不必另开分支或 PR）。

## 常用命令

```powershell
cargo build
cargo run
cargo run -- --self-test    # 创建窗口、绘制一帧后自动退出，退出码 0 表示通过
cargo run -- --bench 5      # 10k 合成队列，输出 tick/帧数/绘制耗时/私有内存
cargo run -- "F:\music"      # 启动时加载文件或目录（也支持拖放）
cargo run -- --page library --library "F:\music"  # 打开曲库页并索引目录
```

方案与指标见 [`docs/WIN32_UI_PLAN.md`](docs/WIN32_UI_PLAN.md)。
