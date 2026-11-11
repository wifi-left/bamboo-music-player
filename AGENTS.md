# AGENTS.md

Bamboo Music Player — Windows 原生音乐播放器，主窗口使用 Win32 + GDI 自绘（Direct2D 仅用于覆盖窗口）。

## 核心：局部重绘

性能来自**只重绘变化的区域**，不是靠重构或降帧：

- 状态变化时只 `InvalidateRect(hwnd, &changed_rect, FALSE)`；`WM_PAINT` 只绘制 `ps.rcPaint` / `GetUpdateRect` 覆盖的区域，不整窗重绘。
- **大状态变化时允许一次整体更新**：页面切换、主题/DPI/窗口尺寸变化、列表批量重建等影响大部分控件的变化，直接整窗失效并重绘，避免碎片化失效和多次 WM_PAINT。
- 列表滚动只绘制新露出的行和滚动条，不重画整个列表。
- 进度条、歌词当前行、时间文本各自维护矩形：只有正在变化的矩形 `InvalidateRect`；
  无动画时停掉 `SetTimer`。
- `IDWriteTextLayout`、封面位图、字体/主题等按版本缓存，禁止每帧重建或重复解码。
- 主窗口全部用 GDI 自绘；可滚动歌词是独立子窗体（`WS_CHILD`，GDI 双缓冲），
  子窗体自己处理滚动/悬停/点击/滚动条/KTV 填充，父窗口只画歌曲卡片与面板外壳。
- 独立歌词窗口（`src/lyrics_window.rs` + `src/lyrics_screen.rs`）：窗口模式用
  原生标题栏/边框，保留系统最小化/最大化/关闭与主题色（不强制改 DWM 配色）；
  F11 切换无边框全屏。窗口壳用独立 `Painter` 自绘，歌词仍是同一个 `lyrics_view`
  子窗体；浮动模式下子窗体必须按自己在父窗口中的偏移重画同一套装饰背景椭圆，
  否则歌词区域会出现与背景不连续的纯色块。窗口无 owner 且 `WS_EX_APPWINDOW`，
  主窗口最小化/隐藏到托盘时仍可显示。
- 播放设置弹窗也是独立子窗体：`SetWindowPos(HWND_TOP)` 置于歌词子窗体之上；
  重叠的自绘子窗体必须带 `WS_CLIPSIBLINGS`，否则失效重绘会越过兄弟窗口，
  表现为“弹窗被歌词条带盖住”。
- 主窗口类不使用 `CS_DBLCLKS`：每次按下都当普通单击处理，快速连点控制按钮
  不会被系统双击窗口吞掉。
- 列表行/滚动条命中必须同时校验页面（仅播放队列/曲库）；其他页面的空白或
  卡片点击不得落成 `PlayIndex`（曾导致正在播放页点卡片时重播/上下一首/随机曲）。
- 播放条封面/歌名：左键跳转“正在播放”；右键弹当前曲目菜单（复制歌名/歌手、
  打开文件位置、打开 URL、复制 Bamboo 歌曲 ID）。菜单使用原生
  `TrackPopupMenu`，数据必须在 `State` 借用结束后克隆并弹出，避免模态循环重入。
- 歌词列表右键：子窗体上报行号（`WM_APP_LYRIC_MENU`），主窗口弹菜单
  （在此处播放/复制文本/复制时间点/重新加载/导入/导出/打开歌词位置）。
  “打开歌词位置”仅本地曲目且存在实际歌词文件时可用；导入/导出对话框在菜单
  返回后执行，不能在菜单回调里持有 `State` 借用。
- 队列/曲库行右键菜单在 `WM_CONTEXTMENU` 中优先于曲目信息菜单；菜单关闭后再
  执行命令（移除/下一首播放/加入队列/清空）。队列页头部提供导入/导出/清空
  快捷按钮，导入导出格式为 `.mp3list`（`core::playlist::mp3list`）。
- 虚拟列表刷新规则：后台刷新必须保留已加载行数（避免滚到深处后只重载第一页而
  空白）；搜索/排序/进页要重置 scroll；任何行数收缩后都要把 scroll clamp 到
  `max_scroll()`。
- 桌面歌词/迷你播放器等跨窗口覆盖面仍用分层窗口（`WS_CHILD` + `WS_EX_LAYERED`，
  由 `app.manifest` 启用）；D2D DC target 画到 DIB 后用
  `UpdateLayeredWindowIndirect` 发布脏条。
- 不要在同一个 GDI 窗口表面上混画 `ID2D1DCRenderTarget`：它会把内部位图
  BitBlt 覆盖整个绑定区域，子矩形绑定还会出现 DWM 不刷新。
- 输入框优先“透明背景、无边框 `EDIT` + 自绘背景”，不要自绘文本编辑器。
- 不要在失去焦点时停止动画（失焦 ≠ 不可见）；按 z-order 计算可见矩形，
  动画脏区与可见区求交，完全遮挡才停止失效/发布；重新可见时整窗重绘。
- 计算遮挡时**不得把 `WS_EX_LAYERED` / `WS_EX_TRANSPARENT` 窗口当作不透明
  遮挡物**（桌面歌词、迷你播放器、tooltip 都可能部分透明）；否则透明区域下方
  的主窗口不会重绘，播放/切换时留下旧像素。宁可多画一点这些窗口下方的动画
  脏区，也不要跳过。
- 小面积覆盖层（桌面歌词/迷你播放器）的 `ID2D1DCRenderTarget` 使用
  `D2D1_RENDER_TARGET_TYPE_SOFTWARE`：硬件 target 会为小 DIB 初始化 D3D/驱动，
  多占约 30MB，收益为零。
- 禁止 `InvalidateRect(hwnd, None, ...)` 式整窗刷新；禁止每帧全量 `BeginDraw` + 全屏绘制。
- 空闲（无动画、无输入）时不应有任何周期重绘。
- 不接受用降帧率、降画质来换占用。

## 性能验收

- 空闲（窗口隐藏、无 overlay）：GPU ≈ 0%，CPU < 1%。
- 主窗口可见、播放 + 滚动歌词：核显 GPU < 3%，UI 线程 CPU < 5%。
- 私有工作集目标 < 100MB。
- 优化前先测量（GPU Engine / 进程 CPU / 工作集 / WM_PAINT 耗时），用数据判断瓶颈。

## 硬性约束

- **只维护一套 UI**：根 crate `bamboo-music-player`（Rust 2024 + Win32 + Direct2D/DirectWrite）。
- `ref/archived/bamboo-egui/` 是 0.1.0 egui 版的只读参考归档：不得作为依赖、不参与根构建、不再同步维护。
- 主窗口使用标准标题栏；不在主窗口重新引入 egui / wgpu / WebView。
- 业务代码（audio / library / lyrics / platform / playlist / api / config）已迁入 `src/core/`；新增业务逻辑放 `src/core/`，不得引用 egui/eframe/winit。
- UI 只读 `Snapshot`、只发 `Command`；UI 线程不做 IO/解码/数据库。
- 大列表必须虚拟化，只绘制可见行。
- 复杂/可滚动/有选择的控件优先用原生子窗体（类似 MFC），子窗体自己处理
  悬停、选择、滚动与擦除；主窗口只绘制静态外壳。
- GDI 绘制必须走 `Painter` 的持久双缓冲，只 blit 脏包围盒；不要直接画到
  `BeginPaint` 的窗口 DC。
- 设置在暂停时也可能变化：离散开关后调用 `AppCore::flush_settings()` 立即
  落盘；连续拖动（音量/进度）仍走节流保存，不能每个鼠标移动写文件。
- 队列筛选必须维护“显示行 → 真实队列下标”的映射；行点击、右键、封面、
  tooltip、当前行高亮都先映射，不能把显示行直接当队列下标。
- 单实例守卫在创建窗口之前执行；第二实例只转发命令行（命名管道）后退出。
  便携模式不写注册表、系统跳转列表，避免污染已安装环境。
- 随机顺序/进度用 track key + cursor 持久化；mp3list 重开时旧顺序保持不变、
  新文件追加到末尾，不得重新洗牌或丢失当前进度。
- 大封面优先 WIC（clipper/scaler）解码，失败再回退 `image` crate；禁止在
  UI 线程解码或在切歌路径上重复解码同一张大图。

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
