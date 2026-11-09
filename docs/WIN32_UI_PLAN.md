# Bamboo Player 原生 UI 迁移方案（Rust + Win32 + Direct2D 脏区）

> 状态：**方案冻结，未实施**  
> 适用前提：Windows-only；不追求与当前 egui 界面像素一致；无障碍（AccessKit）可放弃  
> 目标：GPU/CPU/内存接近 MFC/WPF 级；保留现有音频/曲库/歌词/平台/原生 overlay  
> 最后更新：2026-11-09

---

## 0. 一句话结论

用 **Rust 原生 Win32 窗口 + 原生控件 + Direct2D/DirectWrite 自绘 + `InvalidateRect` 脏区重绘** 替换 egui 主窗口，是当前约束下最能同时压低 GPU、CPU、内存的路线。  
它不是“3 天 egui 效率”的线性延伸：真正的成本在 **把 `App` 从 egui 解耦成 `AppCore`**，以及自建布局/命中/滚动/主题。业务/库代码全部复用，只有 UI 层重写。预计 **6–10 周**（AI 辅助、无像素一致要求；乐观 4–6 周），建议先做 M0 解耦 + M1 单页 spike 再决定全量。

---

## 1. 背景与决策依据

### 1.1 当前架构

- 主窗口：`eframe` + `egui` + `wgpu`，立即模式，每帧整窗 layout + tessellate + 提交。
- 持续动画：迷你播放器/桌面歌词已原生化（D2D 分层窗口 + 脏矩形发布 + 跳帧）。
- 实测（本机 175% DPI，核显）：主窗口隐藏 + 原生 mini：暂停约 0.1%、播放约 1.6% GPU；强制独显反而更高（1.4%/4.2%）。
- 剩余 GPU/CPU 大头：主窗口可见时的 KTV/歌词/动效，以及大列表滚动（当前已虚拟化，但仍是整窗 egui 重绘）。

### 1.2 为什么选原生 Win32 + D2D

- 原生控件 + `InvalidateRect`：只重绘变化矩形，空闲不绘制。
- Direct2D/DirectWrite：硬件加速 2D、文本排版成熟、CJK 支持好；可按矩形裁剪。
- 无 wgpu 交换链、无整窗顶点上传；内存/占用曲线接近 MFC/WPF。
- 现有原生 overlay、托盘、SMTC、IPC、音频引擎全部可复用。

### 1.3 不选其他方案的原因

- Iced：保留控件树，但仍整窗 wgpu 重绘；GPU 不会优于原生 D2D。
- WPF/WinUI3：内存 100–300MB，与低内存目标冲突；且引入 .NET/Windows App SDK。
- WebView2/Tauri/Electron/Flutter：内存 150–400MB。
- 纯手动 Win32 + D2D：性能同级但等于自建 UI 框架，成本高于“原生控件 + 少量自绘”。

### 1.4 复用原则（重要）

**库代码与功能性代码全部复用，只重写 UI 层及其耦合部分。**

| 类别 | 文件/模块 | 处理方式 |
|---|---|---|
| 音频引擎 | `src/audio/*`（约 6.6k 行） | **原样复用** |
| 曲库/索引/扫描/监听 | `src/library/*`（约 2.1k 行） | **原样复用** |
| 歌词解析/服务/编辑器核心 | `src/lyrics/*`（约 2.3k 行，编辑器 UI 除外） | **原样复用** |
| 平台集成（窗口/托盘/SMTC/IPC/注册表/内存） | `src/platform/*`（约 1.9k 行） | **原样复用**；仅主窗口样式/入口按需调整 |
| 歌单/mp3list | `src/playlist/*`（约 1.1k 行） | **原样复用** |
| 电台/下载 | `src/radio/*`、`src/download/*` | **原样复用** |
| 工具/封面/分享图 | `src/util/*` | **原样复用**；封面增加 UI 无关的 RGBA/PNG 输出 |
| 在线 API | `src/api/*` | **原样复用** |
| 配置/会话 | `src/config/mod.rs`、`src/model.rs` | **原样复用** |
| i18n | `src/i18n/*` | **原样复用** |
| 原生 overlay | `src/mini_overlay/*`、`src/desktop_overlay/*` | **原样复用** |
| 播放编排/队列/扫描调度/在线/下载/SMTC/overlay 同步 | `src/app/mod.rs` 的大部分逻辑 | **迁移到 `AppCore`**，去掉 egui/纹理/重绘耦合 |
| 页面 UI | `src/app/views/*`（约 1.1 万行） | **重写**（参考现有布局与交互，不追求像素一致） |
| 控件/主题/字体 | `src/app/widgets/*`（约 0.9k 行）、`app/theme.rs` 调色板、`app/fonts.rs` | 调色板提取复用；控件重写为 D2D；字体改为 DWrite 选择 |
| GPU 背景/egui 启动 | `src/app/gpu_bg.*`、`src/main.rs` 的 eframe 部分 | 替换/删除；背景可简化为纯色/静态图 |
| 独立歌词窗口/迷你播放器 egui 回退 | `src/app/views/lyrics_window.rs`、`mini_player.rs` | 原生 overlay 已存在，可不再迁移 egui 版 |

结论：**不需要重写音频、曲库、歌词、平台、歌单、下载、在线 API、配置等业务代码**；工作量集中在 `app/views`（约 1.1 万行 UI）、`app/widgets` 与 `app/mod.rs` 的 UI 耦合部分。`AppCore` 是“抽取 + 适配”，不是重写业务逻辑。

---

## 2. 目标与非目标

### 2.1 目标

1. Windows 10/11 原生窗口；无 egui/wgpu 主窗口依赖。
2. 空闲时 GPU ≈ 0%、CPU < 1%、无周期重绘。
3. 播放 + 主窗口 KTV 时：核显 GPU < 3%、UI 线程 CPU < 5%。
4. 私有工作集目标：**< 100MB**（含封面缩略图缓存；可按实测调整）。
5. 功能上大致对齐：核心播放/队列/曲库/搜索/歌词/设置可用；允许简化视觉与交互，不要求所有边缘行为 1:1。
6. 允许简化视觉：不要求像素一致、不要求所有动效；优先可用性与性能。

### 2.2 非目标

- 跨平台（macOS/Linux）。
- AccessKit/无障碍（本轮明确放弃；后续可另立项）。
- 与现有主题 100% 一致、复杂动画/模糊背景/流动背景（可保留为可选简化效果）。
- 功能 1:1：只要求核心流程“差不多”，允许删减低频设置项/边缘交互。
- 移动端/Web。
- 重写音频、曲库、歌词解析、下载、平台层。

---

## 3. 成功指标（验收）

| 指标 | 目标 | 测量方式 |
|---|---|---|
| 空闲（主窗口隐藏、无 overlay） | GPU 0.0–0.1%，CPU < 1% | `GPU Engine` 计数器 + 进程 CPU |
| 主窗口可见、暂停 | GPU < 0.5%，CPU < 1% | 同上 |
| 主窗口可见、播放 + KTV | GPU < 3%（核显），UI CPU < 5% | 同上，60s 平均 |
| 列表滚动（1 万行，若测试） | 帧时间 < 8ms，无整窗重绘 | ETW/CPU 采样 + 代码路径 |
| 私有工作集 | < 100MB（含封面缓存） | Task Manager/`Get-Process` |
| 启动到窗口 | ≤ 当前 egui 版本（约 1–2s 内） | 手动/脚本 |

未达标时，优先检查：是否仍在整窗重绘、是否每帧重建文本布局、是否重复解码封面。

---

## 4. 总体架构

```text
┌────────────────────────────────────────────────────────────┐
│  UI 线程（Win32 消息循环）                                  │
│  ├─ 主窗口 / 原生控件 / 自绘面板                            │
│  ├─ AppCore 快照读取 → 布局/绘制                            │
│  └─ 用户操作 → Command 队列                                 │
├────────────────────────────────────────────────────────────┤
│  AppCore（无 egui/winit 依赖）                              │
│  ├─ 播放状态、队列、当前曲目、歌词、设置、下载、在线状态     │
│  ├─ Command 执行、EngineEvent 处理、持久化                  │
│  └─ snapshot() → UI 只读数据                                │
├────────────────────────────────────────────────────────────┤
│  后台线程                                                  │
│  ├─ audio engine（cpal/symphonia，已有）                    │
│  ├─ library scan/watch、cover service、lyrics service       │
│  ├─ download、online API                                    │
│  └─ native mini / desktop lyrics overlay（已有）            │
└────────────────────────────────────────────────────────────┘
```

关键原则：

- **UI 不直接改核心状态**；只发 `Command`，读 `Snapshot`。
- **UI 线程不做 IO/解码/数据库**；图像解码/缩放可在后台，UI 只上传位图。
- **跨线程唤醒**：engine/overlay 事件用 `PostMessage(WM_APP_WAKE)` 唤醒 UI；动画用 `SetTimer` 仅在需要时运行。
- **脏区驱动**：状态变化 → 标记受影响控件矩形 → `InvalidateRect`；`WM_PAINT` 只画更新区。

---

## 5. 代码组织

建议新增：

```text
src/
  core/                     # 从 app/mod.rs 抽出的 UI 无关核心
    mod.rs                  # AppCore, Command, Snapshot, Event
    player.rs               # 播放编排、队列、EngineEvent
    settings.rs             # Settings/Session 读写（复用 config）
    lyrics.rs               # 当前歌词、加载状态
    library.rs              # 曲库/搜索/歌单查询
    images.rs               # 封面/缩略图请求与结果（RGBA/PNG）
  ui_win32/                 # 新原生 UI（仅在 feature 开启时编译）
    main.rs                 # 入口、单实例、日志
    app.rs                  # 消息循环、窗口、AppCore 胶水
    layout.rs               # 简单行/列/网格布局
    theme.rs                # 调色板、字体、DPI 缩放
    paint.rs                # D2D 设备、脏区、双缓冲
    widgets/                # button, list, slider, text, popup, tabs
    pages/                  # now_playing, library, playlists, search, settings...
    d2d/                    # device, text, bitmap, effects, ktv
  app/                      # 现有 egui UI（迁移期保留）
  config/, audio/, library/, lyrics/, platform/, playlist/, ...
```

Cargo：

- 新增可选 feature/binary，例如 `native-ui`，或独立 bin `bamboo-player-native`。
- **不双 UI**：迁移时直接切换；本 egui 版以 Git tag/归档分支保留，不与新 UI 并行发布。迁移期不保留双 UI 对照；本版仅作历史归档。
- 依赖：`windows` 0.62（已有）、`winsafe` 0.0.29（可选）、`rfd`（已有）、`crossbeam-channel`（已有）。
- **本机 Rust 1.93.1**：`windows-canvas 0.100` 要求 1.95，暂不引入；直接用 `windows::Win32::Graphics::{Direct2D, DirectWrite, Dxgi, Imaging}`。升级工具链后再评估 `windows-canvas`。

---

## 6. AppCore 解耦（M0，最关键）

> **M0 不是重写业务逻辑。** 把 `src/app/mod.rs` 中已有的播放/队列/扫描/在线/下载/平台/SMTC/overlay 同步逻辑原样迁移到 `AppCore`，只去掉 egui、纹理、字体和重绘耦合；业务代码以“移动 + 适配”为主。

### 6.1 现状问题

`src/app/mod.rs` 约 1.1 万行，字段与 egui 高度耦合：

- `ctx: egui::Context`、`cover: Option<egui::TextureHandle>`、`background`、`placeholder_cover`、`mini_snapshot` 等。
- UI 函数直接修改状态、直接读纹理/字体。
- 启动/持久化/托盘/IPC/overlay 同步混在同一结构。

### 6.2 拆分原则

1. 新 `AppCore` 不引用 `egui` / `eframe` / `winit`。
2. 保留现有业务逻辑（播放、队列、扫描、歌词、下载、在线 API）原样迁移。
3. UI 通过命令与快照交互，禁止直接访问内部字段。
4. 迁移期用 egui 适配层把 `AppCore` 接到现有 UI；保证功能不回退。

### 6.3 API 草案

```rust
pub struct AppCore { /* audio engine, queue, settings, session, services */ }

pub enum Command {
    PlayPause,
    Next,
    Prev,
    SeekSecs(f64),
    SetVolume(f32),
    ToggleMute,
    PlayIndex(usize),
    AddPaths(Vec<PathBuf>),
    SetPage(Page),
    TogglePlaylist(PathBuf),
    SetLyricsOffset(i64),
    // ... 按页面补充
}

pub struct Snapshot {
    pub page: Page,
    pub playing: bool,
    pub position_secs: f64,
    pub duration_secs: Option<f64>,
    pub volume: f32,
    pub muted: bool,
    pub current: Option<TrackView>,
    pub queue: Arc<[TrackView]>,
    pub lyrics: Option<Arc<LyricsView>>,
    pub lyrics_loading: bool,
    pub library: Arc<[TrackView]>,
    pub playlists: Arc<[PlaylistView]>,
    pub settings_rev: u64,
    // ...
}

impl AppCore {
    pub fn handle(&mut self, command: Command);
    pub fn snapshot(&self) -> Snapshot;
    pub fn poll_events(&mut self) -> Vec<AppEvent>;
}
```

- `Snapshot` 尽量使用 `Arc`/借用式视图，避免每帧克隆上万条曲目。
- 歌词用 `Arc<LyricsView>`，版本号变化才重建。
- 封面：`images.rs` 提供 `request_cover(key)` + `take_results()`；返回 RGBA 或 PNG，UI 负责创建 D2D bitmap（与现有 desktop_overlay 的 PNG 方案一致）。

### 6.4 egui 适配层

- 现有 `App` 保留，内部持有 `AppCore`；egui 页面把操作翻译成 `Command`，从 `Snapshot` 构建视图。
- 先做“只读快照 + 命令”的薄适配，逐步替换直接字段访问。
- 验收：egui 版功能不回退；`cargo test` 全绿。

### 6.5 迁移顺序

1. 抽出纯数据/服务（audio/library/lyrics/download/settings/session）。
2. 抽 `Snapshot` 与 `Command`；egui 页面改为通过它们读写。
3. 把 overlay 同步（native mini/desktop lyrics）、托盘、IPC 移到 `AppCore` 或平台层，UI 只订阅。
4. 删除 `App` 中仅 egui 使用的纹理/字体字段（由 UI 层持有）。

---

## 7. Win32 外壳

### 7.1 窗口与消息循环

- 使用 `winsafe`（推荐）或裸 `windows`：
  - `WindowMain` / 自定义窗口类；`WM_CREATE/WM_SIZE/WM_PAINT/WM_DPICHANGED/WM_TIMER/WM_APP_WAKE`。
  - 单实例/IPC/托盘/跳转列表/SMTC 复用 `src/platform/`。
  - 文件对话框用 `rfd`（原生）。
- 无边框/自绘标题栏：如需现代外观，可用 `WM_NCCALCSIZE` + DWM 圆角/阴影；也可以先用标准标题栏降低首版成本。

### 7.2 布局

- 自建轻量布局：`Row`（水平）、`Column`（垂直）、`Grid`（表格）、`Stack`（覆盖）、滚动容器。
- 所有尺寸用逻辑点（DIP），按 `GetDpiForWindow` 缩放；与现有 overlay 的 DPI 处理一致。
- 固定侧栏宽度 + 可伸缩内容区；窗口最小尺寸约束。

### 7.3 输入/焦点/IME

- 文本输入优先使用原生 `EDIT` 控件（IME、选区、剪贴板、无障碍由系统处理）。
- 自绘控件自行处理鼠标/键盘焦点；Tab 顺序维护一个焦点列表。
- 快捷键：全局热键沿用 `global-hotkey`；局部快捷键在消息循环处理。
- 拖放：`WM_DROPFILES` 或 `IDropTarget`；复用现有 `add_paths`。

### 7.4 主题

- 暗色主题：自定义调色板（从 `src/app/theme.rs` 提取为 UI 无关的 `Palette`）。
- 原生控件暗色化需要 `SetWindowTheme`/owner-draw；**建议首版用自绘控件承担主要视觉，原生 EDIT 仅用于输入**。
- 不追求像素一致：允许简化圆角、阴影、模糊；优先清晰和性能。

---

## 8. Direct2D 渲染与脏区

### 8.1 渲染目标选择

| 方案 | 优点 | 缺点 | 建议 |
|---|---|---|---|
| `ID2D1HwndRenderTarget` | 简单、直接绑 HWND、`BeginPaint/EndPaint`、天然适合 `InvalidateRect` | 设备丢失需重建；大窗口重绘时整窗呈现 | **首版主窗口** |
| `ID2D1DeviceContext` + DXGI swapchain | 更现代、可做复杂效果/交换链 | 需要自己处理 present/脏矩形；与 Win32 更新区配合更复杂 | 后续性能优化/特效 |

首版用 `ID2D1HwndRenderTarget`：

- `WM_PAINT`：`BeginPaint` → 取 `ps.rcPaint` → `rt.BeginDraw` → 按更新区裁剪 → 绘制 → `EndDraw` → `EndPaint`。
- 状态变化：计算受影响矩形 → `InvalidateRect(hwnd, &rect, FALSE)`；仅在需要时 `InvalidateRect(hwnd, None, FALSE)`。
- 双缓冲由 D2D 处理；避免 GDI 闪烁。

### 8.2 脏区策略

- 每个控件/行有稳定矩形；状态变化只标记该矩形。
- 列表滚动：只重绘新露出的行 + 滚动条；不要整列表重绘。
- 动画（进度条/KTV）：只在动画区域 `InvalidateRect`，由 `SetTimer` 驱动；无动画时停 timer。
- 文本/布局缓存：按（文本、字体、字号、宽度、DPI、主题）缓存 `IDWriteTextLayout`；失效时重建。
- 封面：WIC 解码 → `ID2D1Bitmap`；缩略图 LRU；切换曲目时只重绘封面区。

### 8.3 KTV

复用现有 `desktop_overlay/render.rs` 的算法：

- `IDWriteTextLayout` + `HitTestTextPosition` 得到演唱边界；
- 基础文本 + 轴对齐 clip 叠加高亮色；
- 长行水平滚动（已有实现可移植）；
- 逐行/逐字进度由 `position_secs` 驱动。

### 8.4 动画调度

- 统一 `AnimationScheduler`：登记需要 60fps 的控件（进度条、KTV、可视化）；无登记时停止 timer。
- 每秒时间文本只在秒变化时失效。
- 窗口最小化/隐藏/失焦时暂停非必要动画（与现有策略一致）。

### 8.5 原生 overlay

- 迷你播放器、桌面歌词继续使用现有 `src/mini_overlay`、`src/desktop_overlay`；它们已经是 D2D 分层窗口 + 脏矩形发布。
- 新 UI 只负责显示/隐藏、传状态、接收动作；不要合并进主窗口渲染。

---

## 9. 控件策略

| 区域 | 实现 | 理由 |
|---|---|---|
| 侧栏导航/按钮 | 自绘 D2D | 主题一致、圆角/悬停简单 |
| 曲库/队列/歌单列表 | 自绘虚拟列表（或 `ListView` owner-data） | 只绘制可见行；行内封面/标签定制 |
| 搜索框/设置输入 | 原生 `EDIT` | IME、选区、剪贴板稳定 |
| 下拉框/复选框/滑块 | 自绘 D2D（首版） | 暗色主题、尺寸可控 |
| 播放条/进度/KTV | 自绘 D2D | 高频局部更新 |
| 封面/卡片 | 自绘 D2D + WIC bitmap | 圆角裁剪、缓存 |
| 弹窗/菜单 | 自绘 popup 或原生 `TrackPopupMenu` | 首版可用原生菜单降成本 |
| 托盘/任务栏/SMTC | 现有平台层 | 无需重写 |

原则：**输入类优先原生，视觉类优先自绘**；不要试图把原生控件完全暗色化到像素一致。

---

## 10. 页面迁移范围与优先级

| 优先级 | 页面 | 现有文件 | 首版范围 |
|---|---|---|---|
| P0 | 外壳/侧栏/播放条 | `app/mod.rs` | 完整 |
| P0 | 队列/歌单 | `app/views/playlists.rs` | 列表 + 播放/排序/删除/筛选 |
| P0 | 曲库 | `app/views/library.rs` | 列表 + 搜索 + 播放 |
| P1 | Now Playing（简化） | `app/views/lyrics.rs` | 封面/标题/歌词/KTV/收藏 |
| P1 | 在线搜索 | `app/views/search.rs` | 搜索 + 播放 + 下载 |
| P1 | 设置（核心项） | `app/views/settings.rs` | 播放/外观/迷你/歌词/曲库 |
| P2 | 电台/发现/下载/统计 | 对应 views | 功能可用即可 |
| P2 | 歌词编辑器/分享图/大歌词窗 | 对应 views | 延后或直接移除，不保留双 UI |

首版不追求全部设置项；先保证“能播放、能找歌、能管理队列、能改关键设置”。

---

## 11. 里程碑与验收

### M0：AppCore 解耦（1–2 周）

- 抽出 `core`，egui 版通过 `Command/Snapshot` 运行。
- 验收：现有功能不回退，`cargo test` 149+ 通过，egui 版可正常发布。

### M1：原生外壳 Spike（3–5 天）

- winsafe/裸 Win32 窗口 + 侧栏 + 一个虚拟列表 + D2D 播放条 + 一个自绘 KTV 区。
- 脏区：滚动/进度/KTV 只重绘变化矩形。
- 验收：内存 < 100MB、播放 GPU < 3%（核显）、开发手感可接受；否则停止并回退。

### M2：P0 页面（1–2 周）

- 外壳/队列/曲库/播放条完整；原生 mini/桌面歌词联动。
- 验收：常用播放流程可用；空闲 0%、播放 < 3%。

### M3：P1 页面（1–2 周）

- Now Playing 简化版、搜索、核心设置。
- 验收：功能对齐清单；CJK/IME/DPI 通过。

### M4：P2 与收尾（2–3 周）

- 剩余页面/设置项、主题微调、错误提示、回归。
- 验收：完整手工测试矩阵通过；性能指标复测。

总预算：**6–10 周**（含 20–30% 缓冲）。业务/库代码复用，预算主要花在 AppCore 解耦与 UI 重写；若沿用 AI 辅助高强度开发，乐观 4–6 周，但不建议按乐观值排期。

---

## 12. 性能预算与测量

### 12.1 场景

1. 主窗口隐藏、无 overlay（空闲）。
2. 主窗口隐藏 + 原生 mini 播放。
3. 主窗口可见、暂停。
4. 主窗口可见、播放 + KTV。
5. 曲库滚动（1 万行，若保留）。
6. 最小化/恢复、DPI 切换、多显示器。

### 12.2 工具

- `Get-Counter '\GPU Engine(*)\Utilization Percentage'` 按 PID 汇总。
- `Get-Process` 私有工作集；任务管理器对照。
- ETW/PerfView 或 `superluminal`（可选）看 UI 线程热点。
- 代码内计时：`WM_PAINT` 耗时、`D2D EndDraw`、文本布局次数、封面解码次数。
- 基线：当前 egui 版同场景数据（已记录部分）。

### 12.3 常见回退原因

- 每帧 `InvalidateRect(None)` 或整窗重绘；
- 每帧重建 `IDWriteTextLayout`；
- 列表未虚拟化；
- 封面重复解码；
- 动画 timer 在暂停时仍运行；
- 原生控件触发额外 `WM_PAINT`。

---

## 13. 测试矩阵

- DPI：100% / 125% / 150% / 175% / 200%（含混合 DPI 多显示器）。
- 主题：暗色（首版只做暗色；亮色可后补）。
- 文本：中文/日文/英文/长歌词/翻译/逐字 KTV/无歌词。
- 输入：中文 IME、日文 IME、粘贴、拖放文件、快捷键。
- 窗口：最小化/恢复、隐藏到托盘、全屏歌词、多窗口 overlay。
- 播放：本地/在线、切歌、seek、静音、音量、随机/循环、无缝/淡入淡出。
- 曲库：扫描/监听/搜索/歌单导入导出。
- 稳定性：长时间播放、设备切换、睡眠唤醒、D2D 设备丢失。
- 性能：上述 6 个场景 + 60s 采样。

---

## 14. 风险与缓解

| 风险 | 影响 | 缓解 |
|---|---|---|
| AppCore 解耦回归 | 高 | 分阶段、egui 适配层、现有测试、功能清单 |
| 暗色原生控件 | 中 | 视觉控件自绘；输入用原生 EDIT |
| 自建布局/滚动/命中 | 中 | 只做需要的 4 种布局；列表虚拟化 |
| IME/CJK | 中 | 原生 EDIT；DWrite 字体回退测试 |
| 高 DPI/多显示器 | 中 | per-monitor v2；WM_DPICHANGED；复用 overlay 经验 |
| D2D 设备丢失 | 中 | 重建 render target；错误处理 |
| `windows-canvas` 太新/MSRV 1.95 | 低 | 首版用 `windows` 裸 D2D；升级后评估 |
| winsafe 0.0.x API | 低 | pin 版本；必要时裸 `windows` |
| 原生 UI 开发慢 | 高 | 不追求像素一致；先 P0；spike 止损 |
| 失去 AccessKit | 已知 | 明确非目标；后续可补 UIA |

---

## 15. 发布、回滚与兼容

- **不双 UI**：只发布一套 UI。当前 egui 版在本仓库打 tag/归档，原生 UI 完成后直接替换。
- 不提供两个并行发布的可执行文件，也不做双 UI 对照。
- 共享 `settings.json` / `session.json` / 曲库/缓存；字段兼容由 `AppCore` 负责。
- 原生 UI 达到验收后切换默认；回滚通过 Git tag/分支重新构建，不保留运行时双 UI 开关。
- 原生 mini/桌面歌词/托盘/SMTC/IPC 不受 UI 切换影响。
- 不改变配置格式；新增字段保持 `serde(default)`。

---

## 16. 依赖与工具链

- Rust：本机 **1.93.1**；`windows` 0.62（已有）。
- `winsafe` 0.0.29（可选，MIT，活跃；0.0.x）。
- Direct2D/DirectWrite/WIC/DXGI：`windows` crate 的 `Win32_Graphics_*` features（已有大部分）。
- `rfd`（已有）、`crossbeam-channel`（已有）、`global-hotkey`（已有）。
- `windows-canvas 0.100`：**需要 Rust 1.95**，暂不启用；升级工具链后再评估是否替代裸 D2D。
- 不用 `native-windows-gui`（原版停更）；如用其 fork 需自行评估维护风险。
- `piet-direct2d`：冻结/被 Vello 取代，不作为新项目依赖。

---

## 17. 待确认决策

1. 首版是否保留标准标题栏（是：省成本；否：自绘/DWM）。
2. 列表用自绘虚拟列表还是原生 `ListView` owner-data（建议自绘，行内封面/样式更可控）。
3. 封面图解码放 UI 线程还是后台（建议后台 → RGBA/PNG → UI 创建 bitmap）。
4. 设置页是否首版全量（建议只做核心项，其余延后；不保留双 UI）。
5. 是否保留 egui 版作为长期备用（决定：不双 UI；仅用 Git tag/分支归档）。
6. 是否升级 Rust 到 1.95 以使用 `windows-canvas`（建议先不升级，减少变量）。

---

## 18. 参考

- 当前原生 overlay 实现：`src/mini_overlay/`、`src/desktop_overlay/`（D2D、DPI、脏矩形发布可参考）。
- 现有重绘调度：`src/app/mod.rs`（`animation_millis`、`overlay_millis`、heartbeat）。
- 性能与 fork 评估：`HANDOFF.md` §4、§11。
- winsafe: <https://github.com/rodrigocfd/winsafe>
- windows-rs / Direct2D samples: <https://github.com/microsoft/windows-rs>
- windows-canvas: <https://docs.rs/windows-canvas/0.100.0/windows_canvas/>（需 Rust 1.95）
- native-windows-gui（未维护）: <https://github.com/gabdube/native-windows-gui>
- piet-direct2d（冻结）: <https://github.com/linebender/piet>
- damage-rects（仅脏区合并）: <https://github.com/goliajp/rust-damage-rects>
