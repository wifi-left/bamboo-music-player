# Bamboo Music Player — 交接文档（原生 UI / M0）

> 最后更新：2026-11-11（§27）
> 当前分支：`main`
> 当前验证：`cargo build` 0 warning；`cargo test` 227 passed / 0 failed / 6 ignored；
> `--self-test` 全页面 / 设置标签通过；`--bench 3` ticks=189 frames=186
> avg_paint=607us private=28.0MB（debug）
> 构建产物：`target/debug/bamboo-music-player.exe`
> 本机环境：Windows 11（175% DPI）+ Intel RaptorLake 核显 + NVIDIA RTX 5060 Laptop
>
> 2026-11-10 完成六批：真实封面、歌词编辑/分享图、桌面歌词/迷你播放器、
> 系统集成（SMTC/通知/跳转列表/单实例/关闭行为）、设置页（播放/窗口/系统）、
> 队列筛选/排序、随机顺序/歌单进度缓存、大封面 WIC 解码。
>
> 2026-11-10 又完成七批：运行时主题调色板（暗/亮/跟随系统 + 强调色 + 列表
> 密度）、设置页新增“歌词 / 独立歌词 / 桌面歌词 / 迷你 / 外观”分类与通用行
> 滚动/滚动条、歌词与独立歌词字号/颜色/对齐/KTV/words_only 接线、桌面歌词与
> 迷你播放器设置页、原生取色器与数值输入（歌词/桌面歌词颜色、字号/行距等）、
> 10 段均衡器与 ReplayGain 扫描开关、文件夹浏览页（§13–§15）。
>
> 2026-11-10 再补一批：文件夹页后退/前进历史与地址栏、行右键菜单、
> 以及“工具栏按钮悬停后消失”的局部重绘修复（§16）。
>
> 2026-11-10 再一批：曲库整库入队、检查缺失文件、从索引移除、扫描中取消
> 索引（§17）。
>
> 2026-11-10 四批：设置 JSON 导入/导出/恢复默认、界面缩放与动画帧率上限、
> 迷你播放器独立配色、文件夹页封面缩略图（§18）。
>
> 2026-11-10 P2 系统与外观（§20）：应用内 Toast、播放统计页、性能设置界面、
> 模糊封面/流动渐变/Mica 背景、音频可视化。快捷键自定义与无障碍（含键盘/
> 手柄兼容）按用户要求留到最后一批。
>
> 2026-11-10 歌词/播放/曲库续补（§21）：逐面字体/描边/阴影、编码/偏移/
> 翻译语言/预取、A-B 循环、输出设备、自定义睡眠、智能歌单页、查重结果页、
> 文件夹多选与队列拖放排序；修复智能歌单页批量解码封面导致的卡死。
>
> 2026-11-10 在线 / 列表 / 外观（§22）：Bamboo 解析播放 + 流缓存 + 在线歌词、
> 搜索页（防抖/建议/专辑/歌手/随机/URL 入口/行右键）、网络/存储/下载设置、
> 缓存清理、ReplayGain 扫描、查重保留/移除、智能与查重右键、文件夹拖入队列、
> 主题字体/字号、歌词面板底色、桌面逐面字体、迷你高度/双击回主窗口。
>
> 2026-11-11 最后一批（§19/§26）：overlay 事件唤醒与空闲停表、遮挡停发布、
> 主窗口系统/overlay 心跳节流、UI 文案去内部术语、全局快捷键自定义与冲突
> 检测、Tab 焦点环、滑杆方向键、UIA 片段树和 XInput 手柄。实测主窗口播放
> GPU 0%、单核 CPU 4.3%；overlay 暂停时 CPU/GPU 均 0%。详见 **§26**。
>
> egui 0.1.0 版的历史交接与 GPU 调研保留在
> [`ref/archived/bamboo-egui/HANDOFF.md`](ref/archived/bamboo-egui/HANDOFF.md)，
> 仅作参考，不再维护。

---

## 0. 仓库布局

```text
Cargo.toml                  # 根 crate：bamboo-music-player（Win32 + GDI 自绘）
src/
  main.rs                   # 入口、窗口/消息循环、定时器、拖放、托盘唤醒、--self-test/--bench
  app.rs                    # UI 状态：布局、滚动、脏矩形、输入 → Command；读 Snapshot 绘制
  paint.rs                  # GDI 自绘（侧栏/列表/播放条/进度/时间/歌曲卡片/歌词面板）
  lyrics_view.rs            # 滚动歌词子窗体（GDI 双缓冲、滚动/悬停/点击/滚动条/KTV）
  lyrics_window.rs          # 独立歌词窗口（复用 lyrics_view / lyrics_screen）
  lyrics_screen.rs          # 独立窗口沉浸式壳：背景/封面/进度/播放控制
  lyrics_editor_window.rs   # 独立歌词编辑窗口（原生多行 EDIT + 保存/取消、Ctrl+S）
  overlay_host.rs           # 桌面歌词/迷你播放器 host：状态同步、动作→Command、心跳
  player_panel.rs           # 播放设置弹窗子窗体（速度/均衡器/音量/overlay 开关）
  ktv_overlay.rs            # 已停用：旧单行 KTV 分层子窗口（保留作 D2D 参考，不参与构建）
  layout.rs                 # 纯布局矩形（DIP）
  demo.rs                   # 仅 --bench/--self-test 的 10k 队列 + 定时歌词夹具
  core/                     # M0：UI 无关业务核心（本轮的迁移产物）
    mod.rs
    app.rs                  # AppCore / Command / Snapshot / 播放编排 / 持久化 / demo 时钟
    model.rs                # Track / TrackSource / Page / RepeatMode
    config/                 # Settings / Session / Paths / 原子写 JSON
    audio/                  # cpal + symphonia 引擎、HLS、变速、响度、指纹
    library/                # SQLite 曲库、扫描、目录监听、文件夹浏览
    lyrics/                 # LRC/SRT、歌词服务、编辑器、view（行布局/当前行/命中）
    playlist/               # Queue（随机/循环/去重）、mp3list
    api/ download/ radio/   # 在线 API、下载、电台
    util/                   # 封面（RGBA）、元数据、分享图、图标
    platform/               # tray / ipc / smtc / jump_list / native_overlay / native_mini
    desktop_overlay/        # 原生桌面歌词窗口
    mini_overlay/           # 原生迷你播放器窗口
    overlay_protocol.rs     # 主线程 ↔ 原生 overlay 协议
ref/archived/bamboo-egui/   # 0.1.0 egui/eframe 版：只读参考归档，不参与根构建
docs/                       # WIN32_UI_PLAN.md / NATIVE_UI_EXPERIENCE.md
scripts/                    # i18n 维护脚本（从归档版移到根目录）
```

根 crate 为了避免一次性改几千处 `crate::model` / `crate::config` 路径，在
`src/main.rs` 里对 `core` 做了模块再导出：

```rust
mod core;
pub use self::core::{
    api, audio, config, desktop_overlay, download, i18n, library, lyrics,
    mini_overlay, model, overlay_protocol, platform, playlist, radio, util,
};
```

新增代码应优先写 `crate::core::...`；保留再导出是为了让迁移过来的业务代码保持原样。

---

## 1. 本轮完成

### 1.1 归档目录改名

- `crates/bamboo-egui/` → `ref/archived/bamboo-egui/`（用户要求）。
- README / AGENTS.md / `docs/WIN32_UI_PLAN.md` / 归档 README 中的路径已同步。
- 归档版仍然可以单独构建，但不是根 crate 的依赖。

### 1.2 M0：业务核心迁入 `src/core/`

- 迁入 audio / api / config / library / lyrics / playlist / radio / download /
  util / i18n / platform / desktop_overlay / mini_overlay / model / overlay_protocol。
- **全部业务模块不引用 egui/eframe/winit**；`cargo build` 0 warning。
- egui 相关适配只有两处：
  - `util::image_util` 定义 `RgbaImage { size, pixels }` 取代 `egui::ColorImage`；
    `util::cover` 的 `encode_png` 直接写 RGBA。
  - `platform::tray` 去掉 `egui::Context`，改为 host 提供的 `wake` 回调。
- 没有迁入 `platform::window`（原生外壳自己管窗口）与 `app/`、`app/views`、
  `app/widgets`、`app/theme`、`app/fonts`、`gpu_bg`（都是 egui UI）。
- 归档版的 `scripts/` 复制到根目录（i18n 测试仍需要 `i18n_strings.json`）。

### 1.3 AppCore：真实播放与持久化

`src/core/app.rs`：

- `AppCore` 持有 `Settings` / `Session` / `Paths` / `AudioEngine` / `Queue` /
  `LyricsService` / 当前曲目 / 播放状态 / 会话脏标记。
- `Command`：PlayPause / Next / Prev / Stop / SeekSecs / SetVolume / ToggleMute /
  PlayIndex / AddPaths / ToggleShuffle / CycleRepeat / ClearQueue / SetPage。
- `Snapshot`：只读队列、当前曲目、状态、位置/时长、音量/静音、随机/循环、
  当前歌词与加载状态。
- 已实现：
  - 本地文件、直链 URL/电台播放；gapless 预开下一首（`Queue::peek_next` +
    `AudioEngine::set_next`/`AutoAdvanced`），轨道结束自动下一首；
  - 歌词后台加载（`LyricsService`），翻译区域沿用 `resolve_region`；
  - 会话恢复（`resume_on_start`）、设置/会话周期自动保存、退出保存；
  - 相对归档版的曲目缺失跳过、随机/循环、音量/静音逻辑。
- **Bamboo 在线音源尚未接入**：`TrackSource::Bamboo` 目前只记录告警并停止播放，
  下一步要接 `api.resolve` + 流缓存 + 在线歌词兜底。

### 1.4 原生 UI 接到 AppCore

- `src/app.rs` 不再使用演示曲目：队列、当前曲目、歌词、播放状态全部来自
  `AppCore` 快照；输入转换成 `Command`。
- 播放条、Now Playing、KTV 下一行提示都读真实数据；KTV 子窗口用
  `app.lyric_line(index)` 取当前行文本。
- `--bench` 保留 10k 合成队列 + 定时歌词：`App::enable_demo()` →
  `AppCore::load_demo()`，内部有独立 demo 时钟，**不会写回用户会话**。
- 新增：
  - `WM_DROPFILES` 拖放文件/文件夹到窗口加入队列；
  - 命令行参数中的文件/目录在启动时加入队列（空队列自动播放第一首）；
  - `DragAcceptFiles(hwnd, true)`。

### 1.5 托盘

- `src/core/platform/tray.rs` 已迁入并接回原生主窗口：
  - `WM_APP_WAKE` 由托盘/音频事件线程 `PostMessage` 唤醒，不轮询；
  - 显示/隐藏/切换窗口、播放/暂停、上一首/下一首、音量、随机/循环、
    桌面歌词/锁定开关、退出；
  - 退出项设置 `quit_request` 后直接 `PostMessage(WM_CLOSE)`；
    `WM_CLOSE` 在 `quit_request` 为真时 `DestroyWindow`，否则隐藏到托盘。
- 托盘菜单里桌面歌词/锁定/随机/循环的勾选状态目前是主窗口 `State` 内的
  演示/占位状态，等 overlay/设置页接入后再与 `AppCore` 真正联动。

### 1.6 其他修正

- `util::share_image` 的像素转换从 `Color32` 改为扁平 RGBA。
- `include_bytes!/include_str!` 相对路径按新目录修正（`assets/`、`scripts/`）。
- `.gitignore` 为 `HANDOFF.md` 增加例外，交接文档可以纳入版本管理。
- **MFC 风格视觉样式**：`app.manifest` 已内嵌
  `Microsoft.Windows.Common-Controls 6.0.0.0` 依赖与 PerMonitorV2 DPI；
  启动时调用 `InitCommonControlsEx(ICC_STANDARD_CLASSES | ICC_WIN95_CLASSES)`。
  原生 `EDIT`/`LISTBOX`、菜单、消息框使用系统视觉样式；主窗口/歌词/播放条
  等自绘区域仍保持应用自己的深色主题。

---

## 2. 验证记录

```powershell
cargo build          # 0 warning
cargo test           # 165 passed / 0 failed / 5 ignored
cargo run -- --self-test
cargo run -- --self-test --page library|queue|settings
cargo run -- --bench 5
```

- `--self-test`：`painted=true`，退出码 0；四个页面各自绘制通过。
- `--bench 4`（10k 队列，debug，175% DPI）：
  `ticks=158, frames=163, avg_paint=520us, private=14.0MB ws=35.5MB`
  （不同轮次约 0.4–0.5ms/帧；`--bench`/`--self-test` 不打开曲库 DB/索引，
  不会触碰用户数据或索引进程）。
- 真实曲库验证（本机 10 255 首，SQLite）：首屏 2000 行，滚动到底自动加载下一页；
  搜索 `Time` 过滤为 1 行；列表最后一行被裁剪在播放条之上。
- `--bench`/`--self-test` 使用 `LibraryDb::disabled()` / `WatchService::disabled()`：
  不打开 SQLite、不启动监听线程、不写用户数据。
- 内存：M1 纯外壳约 6MB；M0 播放/歌词约 14MB；接入曲库 DB/监听后仍 ≈14–18MB，
  远低于 100MB 目标。
- 未在真实声卡上验证：本会话没有可听的音频输出环境；解码/加载路径已通过
  命令行 WAV 冒烟（无引擎错误日志）。发布前需在真实桌面验证播放/seek/gapless。

---

## 3. 下一步（优先顺序）

> 完整的功能差距与建议顺序见 **§8**（2026-11-09 对照 0.1.0 归档版整理）；
> 本节保留早期迁移计划，部分条目已在后续完成。

1. ~~**P0 曲库页**：列表/搜索/排序/索引/目录管理~~ **已完成（见 §5）**。
2. **P0 队列/歌单页**：真实 `Queue` 的删除/拖动排序/筛选；
   `playlist::mp3list` 导入导出；`Session::playlists`；
   暴露 `Command::LibraryQueueAll`（已在核心实现，UI 尚未接入）。
3. **正在播放重做 + 滚动歌词组件（可复用）**：**已完成（见 §6.8、§6.9、§10.2）**：
   歌曲卡片 + 歌词面板 + 可滚动/可点击歌词子窗体、大歌词模式、独立歌词窗口、
   歌词编辑窗口。桌面歌词/迷你播放器已接入（§10.3）。
   - ~~新增 UI 无关的 `core::lyrics::view`~~ **已完成**：行高/当前行/滚动偏移/
     命中测试/点击跳转/翻译行/KTV 进度；渲染仍由各表面自己做。
   - ~~主窗口用独立子窗体承载歌词列表~~ **已完成**：滚动、hover、点击、自动居中
     都在子窗体内完成，父窗口只画卡片与面板外壳。
   - 桌面歌词/迷你播放器当前是单行布局，接入后复用同一 view；迷你播放器可保留
     “紧凑单行”模式作为设置项。
   - 动画只在当前行变化时滚动；暂停/无时间轴/关闭 KTV 时不按帧重绘；
     遮挡时停止发布（沿用现有 overlay 规则）。
4. **在线 Bamboo**：`api.resolve` → 流缓存 → `EngineSource::Url`；
   快速切歌的歌词兜底（参考归档版 §3.4）。
5. **设置页**：曲库/播放/窗口/系统四分类已完成（§11.2）；外观/主题/缩放/
   迷你播放器详细项仍待后续。
6. **原生 overlay 接回 AppCore**：**已完成（§10.3）**。`overlay_host` 负责
   快照→`OverlayState`/`MiniState`、动作→`Command`、250ms 位置心跳；播放/暂停
   变化强制立即发状态（修复暂停后图标不刷新）。
7. **封面/背景**：**已完成（§10.1）**。`CoverService` + 行缩略图缓存接入播放条、
   歌曲卡片、队列/曲库列表、独立歌词窗口、桌面歌词/迷你播放器与 SMTC PNG。
8. **SMTC / 单实例 / 跳转列表**：**已完成（§11.1）**，含关闭行为、捕获保护、
   开机启动/文件关联与第二实例转发。
9. 清理 `AppCore` 中 `#[allow(dead_code)]` 的 Command/Snapshot 字段（页面接入后）。

---

## 4. 重要约定（沿用并补充）

- **只有一套 UI**：根 crate 是唯一实现；`ref/archived/bamboo-egui` 只读。
- UI 只读 `Snapshot`、只发 `Command`；UI 线程不做 IO/解码/数据库。
- 大列表虚拟化；`WM_PAINT` 只画 `rcPaint` 相交元素；禁止整窗无效化。
- 主窗口非 KTV 区域 GDI；KTV 独立分层子窗口 + 软件 D2D DC target +
  `UpdateLayeredWindowIndirect` 脏条发布；不要在同一 GDI 表面混画 D2D。
- 原生窗口线程必须 `SetThreadDpiAwarenessContext(PER_MONITOR_AWARE_V2)`；
  175% DPI 下 layered window 的鼠标 `lparam` 按屏幕坐标处理
  （`GetCursorPos + GetWindowRect` 换算）。
- 托盘/overlay/音频事件都通过 `PostMessage(WM_APP_WAKE)` 唤醒 UI；不要恢复周期轮询。
- 持久化用 `Track::dedupe_key()`，不要用队列下标。
- 新增可选功能必须有设置开关，并能运行时关闭；文案走 `crate::i18n::tr` / `trf!`，
  同步 `src/core/i18n/en.json`。
- 提交前至少：`cargo build`、`cargo test`、`--self-test`。


### 4.1 UI 规则总表（2026-11-09 当前实现）

> 详细踩坑与实测：[`docs/NATIVE_UI_EXPERIENCE.md`](docs/NATIVE_UI_EXPERIENCE.md)。

**架构 / 控件**

- 主窗口：标准标题栏；非 KTV 区域全部 **GDI 自绘**（背景、侧栏、虚拟列表、
  播放条、进度/时间、Now Playing、下一行歌词提示）。
- KTV/滚动歌词：**独立分层子窗口**（`WS_CHILD` +
  `WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE`）；内部
  `ID2D1DCRenderTarget`（**软件渲染**）画到持久 DIB，`UpdateLayeredWindowIndirect`
  的 `prcDirty` 只发布变化条带；父窗口驱动更新，子窗口无计时器。
- 分层子窗口依赖 `app.manifest`（Win8/8.1/10+ `supportedOS`）；样式必须在
  创建后用 `SetWindowLongPtrW(GWL_EXSTYLE, ...)` 设置，不能直接传给
  `CreateWindowExW`（子窗口会 `ERROR_INVALID_HANDLE`）。
- 输入框：透明背景、无边框 `EDIT` + 自绘背景；不自绘文本编辑器。
- 不追求像素级还原；允许简化圆角/阴影/动效。

**重绘 / 脏区**

- 状态变化只 `InvalidateRect` 受影响矩形；`WM_PAINT` 只画更新区/脏矩形相交元素。
- **大状态变化允许整体更新**：页面切换、主题/DPI/窗口尺寸、列表批量重建等，
  一次重绘所有可见区域；不要拆成大量碎片失效。
- 列表虚拟化：只布局/绘制可见行；滚动只重画列表区域与滚动条。
- 动画：进度条只失效填充边缘；KTV 只发布 `old_fill → new_fill` 条带；
  时间文本只在显示秒变化时失效；变化 <0.3 DIP 跳过。
- **时间/进度差值必须用上一 tick 的基准值**：真实播放的位置在音频线程推进，
  同一 tick 内的前后采样只差几微秒，会导致进度边缘与秒数永不失效
  （见 §5.9）。seek/切歌后重置基准。
- 无动画、无输入时停止计时器；暂停/最小化不产生周期重绘。
- 文本布局、画刷、字体、封面位图按版本缓存；禁止每帧重建/重复解码。

**遮挡 / 后台（重要）**

- **不要用“失焦”判断不可见**；失焦但可见时继续动画。
- 每约 0.5s 按 z-order 计算可见矩形：`GetWindow(GW_HWNDPREV)` 枚举本窗口之上的
  顶层窗口，把其矩形从客户区减去；全部减完 = 完全遮挡。
- 动画脏区与可见矩形求交；父窗口只重绘露出部分，KTV 只发布可见条带。
- 完全遮挡：停止所有失效/发布（模拟时钟/播放位置仍推进）；重新可见时整窗
  重绘一次，并强制 KTV 全条带重发，避免露出旧内容。
- 实测：前台/部分遮挡 0.2–0.5% GPU；完全遮挡 **0.00%**。
  （被遮挡窗口会失去 direct/independent flip，DWM 合成 + ULW 整层更新会让
  后台 GPU 反而更高；分层子窗口也不会被遮挡剔除。）

**D2D / GDI 互操作约束**

- **不要在同一 GDI 窗口表面混画 `ID2D1DCRenderTarget`**：它先画到内部位图再
  BitBlt 到绑定区域，会整块覆盖；只绑脏子矩形还会出现 DWM 不刷新。
- 小面积覆盖层的 D2D DC target 用 `D2D1_RENDER_TARGET_TYPE_SOFTWARE`：
  硬件 target 会为小 DIB 初始化 D3D/驱动，固定多占约 30MB。
- 验证局部刷新必须用桌面截屏（`CopyFromScreen`），不能只看 `PrintWindow`。

**性能验收（debug，175% DPI，核显）**

- 空闲/暂停：GPU ≈ 0%，无周期重绘。
- 播放 + KTV：GPU 0.2–0.5%（目标 <3%）。
- 完全遮挡：GPU 0.00%。
- 私有提交：M1 纯外壳约 6MB；M0 接入音频/歌词/SQLite 后约 14MB；
  工作集约 23–35MB（目标 <100MB）。
- 平均绘制约 0.38–0.42ms/帧（`--bench`）。

**线程 / 数据**

- UI 只读 `Snapshot`、只发 `Command`；UI 线程不做 IO/解码/数据库。
- 托盘/overlay/音频事件用 `PostMessage(WM_APP_WAKE)` 唤醒；不要恢复周期轮询。
- 原生窗口线程 `SetThreadDpiAwarenessContext(PER_MONITOR_AWARE_V2)`；
  175% DPI 下 layered window 鼠标坐标用 `GetCursorPos + GetWindowRect` 换算。
- 持久化用 `Track::dedupe_key()`，不要用队列下标。
- 新增可选功能必须有设置开关并可运行时关闭；文案走 `i18n::tr` / `trf!`。

**提交前**

```powershell
cargo build
cargo test
cargo run -- --self-test
cargo run -- --bench 5
```

### 4.2 交互状态与滚动条规范（2026-11-09）

> 所有自绘控件统一遵守；原生子控件（EDIT/LISTBOX）按系统行为补样式。
> 当前实现见 §5.7；滚动歌词的同类要求见 §6。

**总则**

- 可点击控件必须有 `Normal / Hover / Pressed`；可开关控件再加 `Active`
  （开）与 `HoverActive`（开 + 悬停）。
- **禁用控件置灰且不进入命中测试**：不可悬停、不可点击；不要用“点了没反应”代替禁用。
- 悬停只重绘旧/新控件的矩形；按下/释放同样只重绘该控件。禁止整窗失效。
- 鼠标离开窗口（`WM_MOUSELEAVE`）必须清除 hover；页面切换、滚动、列表重建
  要清除可能失效的 hover 目标。
- 按下在 `WM_LBUTTONDOWN` 立即显示 pressed；`WM_LBUTTONUP` 恢复
  hover/normal。拖动（进度条/滚动条）期间保持 pressed 样式。
- 子窗体（LISTBOX/歌词子窗体）自己处理 hover/选择/滚动/擦除；父窗口不参与
  其内部重绘。子窗体隐藏后父窗口必须立即重绘其覆盖区域。
- 文本输入只用原生 `EDIT`（IME/选区/剪贴板由系统处理）；不自绘编辑器。

**颜色（当前值，DIP 无关）**

| 用途 | 颜色 |
|---|---|
| 按钮 Normal | `rgb(45,48,53)` |
| 按钮 Hover（关） | `rgb(58,62,69)` |
| 按钮 Active（开） | `rgb(44,78,64)` |
| 按钮 HoverActive（开+悬停） | `rgb(61,105,86)` |
| 按钮 Pressed | `rgb(36,70,58)` + 强调文字色 |
| 导航 Hover / Pressed | `rgb(40,43,48)` / `rgb(52,86,72)` |
| 列表行 Hover / Pressed | `rgb(37,40,45)` / `rgb(50,76,65)` |
| 当前行/选中行 | `rgb(43,61,55)` |
| 播放按钮 常态/Hover | `rgb(51,79,68)` / `rgb(61,94,81)` |
| 滚动条轨道/滑块 | `rgb(41,43,48)` / `rgb(92,98,106)` |
| 滚动条滑块 Hover/Pressed | `rgb(126,134,144)` |
| 普通文字/弱化文字/强调文字 | `rgb(232,235,238)` / `rgb(158,166,176)` / `rgb(143,224,181)` |

**控件矩阵**

| 控件 | Hover | Pressed | Active/备注 |
|---|---|---|---|
| 侧栏导航 | 是 | 是 | 当前页用选中色；hover 不覆盖选中 |
| 播放队列/曲库列表行 | 是 | 是 | 当前播放行选中色优先；悬停 450ms 后原生 tooltip |
| 曲库翻页 | 上一页/下一页按钮（含禁用态） | — | 每页行数随列表区高度动态计算；PgUp/PgDn/Home/End 也可翻页；无滚动条 |
| 页头 排序/添加目录/重新扫描 | 是 | 是 | 排序菜单打开时按钮为 Active |
| 播放条 上一首/下一首/随机/循环 | 是 | 是 | 随机/循环开启时图标用强调色；循环含单曲图标 |
| 播放条 播放/暂停 | 是 | 是 | 34×34 方形底色，比左右略大、同行垂直居中 |
| 播放条 音量滑块/静音 | 是 | 拖动时 Pressed | 拖动 `SetCapture`；静音时填充变灰 |
| 播放条 进度条 | 是 | 拖动时 Pressed | 仅 4 DIP 条本身可拖动；`SetCapture` + 本地预览，松手才 seek；轨道/填充变亮、条高 4→6→8、细竖游标 |
| 播放条 播放设置/播放列表 | 是 | 是 | 设置弹窗打开时齿轮 Active；播放列表切到队列页 |
| 播放设置弹窗（速度/EQ/音高） | 是（子窗体自绘） | 是 | 开/关不同底色；点击外部/ESC 关闭 |
| 设置 开关（扫描/监听/符号链接） | 是 | 是 | 开/关使用不同 Hover 色 |
| 设置 查重（三段选择） | 是 | 是 | 每个选项 Active 独立 |
| 设置 添加目录 / 移除 | 是 | 是 | — |
| 原生 EDIT（搜索/扩展名） | 不适用 | 不适用 | 聚焦时边框变强调色 |
| 排序 LISTBOX 项 | 是（子窗体自绘） | 系统选择 | 选中绿色、hover 浅灰 + 文字提亮 |
| 歌词行（§6） | 是 | 是 | 当前行强调色；点击 seek |
| 未接功能的按钮（桌面歌词/编辑等） | 否 | 否 | 置灰，无命中目标 |

**滚动条规范**

- 自绘滚动条（主列表、曲库、未来歌词列表）：
  - 内容不足一屏时隐藏；否则显示右侧 8–10 DIP 轨道。
  - 滑块最小 24 DIP；点击轨道/滑块即开始拖动；拖动把光标映射到
    `scroll / max_scroll`，跟随鼠标；悬停和拖动时滑块颜色变亮。
  - 滚轮固定 3 行/格；键盘支持 Home/End/PgUp/PgDn。
  - 滚动只失效列表区 + 滚动条，不整窗重绘。
- 原生子窗体滚动条（排序 LISTBOX）：
  - 使用 `WS_VSCROLL`，内容超出高度时由系统提供，可拖拽。
  - 深色主题用 `SetWindowTheme(..., "DarkMode_Explorer", ...)`（不可用时回退系统色）。
- 禁用/不可滚动状态不显示 hover 反馈。

**键盘与焦点**

- 主窗口：空格播放/暂停，左右 seek，上下/PgUp/PgDn/Home/End 滚动列表。
- 原生 EDIT 聚焦时键盘归 EDIT（空格用于输入，不触发播放）。
- 临时层（排序菜单等）打开时，点击外部关闭；ESC 关闭（排序菜单的 ESC 待补）。
- 拖动进度条/滚动条时建议 `SetCapture`，保证光标移出窗口仍能收到
  `WM_MOUSEMOVE/WM_LBUTTONUP`（当前未设置，列入后续修正）。

**验收**

- 悬停/按下/离开不留下残影；控件状态在一个重绘帧内切换。
- 滚动条可拖拽，滑块与滚动位置一致；滚轮/键盘同样有效。
- hover、按下、滚动均不触发整窗重绘；暂停时无周期重绘。
- 子窗体（排序列表/歌词列表）关闭后父窗口覆盖区域立即恢复。
- 禁用按钮不响应 hover/click。

**当前状态**

- 已实现：导航/列表行/页头按钮/播放控制/设置项/排序 LISTBOX 的
  hover、pressed、on/off 区分；主列表滚动条拖拽；排序 LISTBOX 溢出时的
  原生滚动条；排序菜单 ESC 关闭；系统擦除关闭 + GDI 双缓冲。
- 待实现：歌词列表 hover/滚动条（§6）；未接功能按钮的统一置灰样式。
  （进度条/音量/列表滚动条拖动均已 `SetCapture`。）

---

## 5. P0 曲库页 + 交互样式（2026-11-09 晚）

### 5.1 曲库页

- `AppCore` 新增 `LibraryState`：`LibraryDb`（SQLite）+ `WatchService` +
  查询分页 + 索引进度 + 设置快照；`AppCore::new(..., load_library)` 控制是否启动
  曲库服务（`--bench`/`--self-test` 传 false，避免索引用户曲库）。
- 查询：`LibraryQuery { search, sort, offset, limit }`，每页 2000 行；
  列表仍虚拟化，滚动接近底部自动 `LibraryLoadMore`。
- 搜索：无边框原生 `EDIT` + 220 ms 输入防抖；查询结果用 request id 丢弃过期响应。
- 排序：`LibrarySort`（标题/歌手/专辑/加入时间/时长/播放次数/最近播放/路径），
  页头按钮循环切换。
- 目录：`添加目录`（rfd 原生文件夹选择器，延迟到点击处理返回后打开，避免在
  `State` 可变借用期间重入消息循环）、`重新扫描`、目录列表 + `移除`。
- 播放：点击行 → 队列中已有则跳转，否则加入并播放；统计开启时写 `record_play`。
- 索引事件通过 `LibraryDb`/`WatchService` 的 wake 回调 `PostMessage(WM_APP_WAKE)`
  唤醒 UI，暂停时也能更新进度/结果，不轮询。
- 真实验证：10 255 首曲库首屏 2000 行、滚动加载下一页、搜索 `Time` → 1 行。

### 5.2 设置 → 曲库

页头标题下统一为左对齐小字副标题（与曲库页一致）。设置页已提供：

- 扫描启动时索引 / 自动监听目录 / 索引时跟随符号链接（开/关）
- 查重方式：元数据 → 文件路径 → 音频指纹
- 文件扩展名（原生 EDIT，450 ms 防抖；变更后同步监听并重新索引入口）
- 音乐目录列表 + 移除；添加目录
- 所有改动走 `Command`，`settings_dirty` 后自动保存；设置项没有被删减，
  仍保留在 `Settings.library` 数据模型中。

### 5.3 闪烁修复（连续点击下一首）

- 根因：`next/prev` 与 `App::tick` 在轨道/状态变化时调用 `invalidate_all`，
  连续切歌导致整窗反复重绘；GDI 直绘时表现为闪烁。
- 修复：`App::tick` 使用 `TickState` 对比前后快照（索引/播放态/时长/位置/
  `lyrics_rev`/`library_rev`），只失效：
  - 播放条封面与标题、进度/时间；
  - Now Playing 封面/标题/元信息/KTV 条带；
  - 队列页旧/新行；
  - 歌词变化 → KTV；曲库变化 → 列表/滚动条/页头。
- `next/prev` 与列表点击同样走 `invalidate_track_change`，不再整窗刷新；
  整窗重绘只保留给页面切换、尺寸/DPI 变化和遮挡状态变化。
- 引擎/歌词/曲库事件增量版本号：`Snapshot::lyrics_rev`、`Snapshot::library_rev`。

### 5.4 输入框与控件样式

- 新增 `src/ui.rs`（可复用 UI 基础模块）：
  - `Target`（可悬停/点击区域）、`Input`（原生输入框）、
    `ButtonState { Normal, Hover, Pressed, Active }`；
  - `Interaction` 统一维护 hover/pressed/focus，返回旧/新目标供精确失效。
- 搜索框/扩展名输入框：
  - 无边框 `EDIT` + `WM_CTLCOLOREDIT` 透明背景与自绘边框；
  - 聚焦时边框变强调色；左右 8 DIP 内边距（`EM_SETMARGINS`）；
  - EDIT 在 26 DIP 高的框内上下内缩 3 DIP，文字垂直居中；
  - 搜索框使用 `EM_SETCUEBANNER`（`app.manifest` 增加 Common-Controls v6）。
- hover/pressed/active：
  - 侧栏导航悬停底色；列表行悬停底色；播放控制上一首/播放/下一首；
  - 页头排序/添加目录/重新扫描；设置开关（开=强调底色）、添加目录、移除；
  - 鼠标离开窗口（`WM_MOUSELEAVE` + `TrackMouseEvent`）清除 hover 并只重绘旧区域。

### 5.5 其他 UI 修正

- `HEADER_HEIGHT` 56 → 64 DIP，标题基线下移，顶部留出空间；各页副标题统一左对齐，
  位于标题下方。
- 列表行绘制用 `IntersectClipRect` 裁剪到列表区域；最后一行不再压到播放条。
- 曲库行显示标题/歌手/时长；Now Playing 在无歌手/专辑时不再显示孤立的 `·`。

### 5.6 新增启动参数（便于测试）

```text
--page now|queue|library|search|settings   启动时切页
--library <dir>                            把目录加入曲库并索引
--bench [秒] / --self-test                 不加载曲库 DB/索引
```

### 5.7 双缓冲与复杂控件（2026-11-09 晚二）

**GDI 双缓冲**

- `Painter` 持有持久内存 DC + 兼容位图（随客户区尺寸/DPI 重建）。
- 所有绘制先画到内存位图，只把脏矩形的**包围盒** `BitBlt` 到窗口 DC；
  局部重绘不再出现半帧/闪烁。
- `WM_ERASEBKGND` 返回 1、`InvalidateRect(..., false)`：系统不做背景擦除，
  背景由自绘直接覆盖。
- `Drop` 释放内存位图/DC、画刷和字体。
- 整窗重绘仍只用于页面切换、尺寸/DPI、遮挡状态变化；尺寸变化时
  `App::reset_visibility()` 清空旧的遮挡矩形，避免旧进度条节点残留在新区域。

**排序选择器改为子窗体（MFC 风格）**

- 新增原生 `LISTBOX` 子窗体（`WS_CHILD | LBS_NOTIFY | LBS_OWNERDRAWFIXED |
  LBS_HASSTRINGS | LBS_NOINTEGRALHEIGHT | WS_VSCROLL`），不再是父窗口上的浮层。
- 深色 owner-draw（`WM_DRAWITEM`）；`SetBkMode(TRANSPARENT)` 修掉文字白色底；
  选中项绿色、悬停项浅灰、悬停文字提亮。
- 子类化 `WM_MOUSEMOVE/WM_MOUSELEAVE`（`LB_ITEMFROMPOINT`）跟踪悬停并只
  invalidate 子窗体；`WM_CTLCOLORLISTBOX` 使用与面板一致的背景刷。
- 列表高度精确贴合 8 项（+2 DIP）；窗口过矮时高度截断并显示原生滚动条
  （可拖拽），不会再因差 2px 而选中最后一项时滚动一行、顶部消失/底部留黑。
- 关闭列表（再次点按钮或选择项）时检查子窗体实际可见性并立刻
  `InvalidateRect + UpdateWindow` 重绘父窗口对应区域，消除关闭残影。

**其它选择控件**

- 查重方式由循环按钮改为三段式选择（元数据 / 文件路径 / 音频指纹），
  当前项高亮。
- `src/ui.rs` 的 `ButtonState` 增加 `HoverActive`：布尔按钮“开”和“关”
  使用不同悬停色；按下态使用强调文字色。

**播放条时间/进度**

- 时间文本从进度条的 `hits(progress_hit)` 判断中拆出，按
  `layout.time_left/time_right` 各自失效，秒变化即可更新。
- 主列表右侧自绘滚动条支持点击/拖动（`dragging_scrollbar`），悬停/按下有颜色。

**其它修正**

- 最小窗口尺寸 860×560 → 640×460 DIP，适配 175% DPI 小屏。
- 父窗口 KTV 区域每帧只失效“下一行提示”小条（96 DIP），KTV 条带由分层
  子窗口发布，避免每帧重绘整块歌词区。

**后续方向：滚动歌词组件（用户提出，已列入 §3 第 3 项）**

- “正在播放”区域将替换为**可滚动、可点击的全部歌词**（原 egui 版行为）。
- **桌面歌词与迷你播放器复用同一套逻辑**：两者现在都是单行/D2D overlay，
  接入后共用 `core::lyrics::view` 的行布局、当前行、滚动偏移、命中/点击跳转、
  翻译行与 KTV 进度；渲染仍在各自表面完成（mini 可保留紧凑单行模式）。
- 承载方式：
  - 主窗口：独立子窗体（MFC 风格）或独立表面承载整块歌词列表；
  - 桌面歌词/迷你播放器：继续用现有分层窗口 + 软件 D2D 表面。
  - 滚动、hover、点击跳转、自动居中都在歌词表面内完成；父窗口只在页面/
    主题/尺寸变化时重绘，**不要把可滚动歌词列表直接画进主窗口脏区体系**。
- 动画规则：只在当前行变化时滚动；暂停/无时间轴/关闭 KTV 时不按帧重绘；
  被完全遮挡时停止失效/发布，恢复可见时整块重绘。

### 5.8 已知限制

- 曲库页暂未暴露“全部入队”（核心 `LibraryQueueAll` 已实现）。
- 排序子窗体的滚动条使用系统样式（`DarkMode_Explorer` 可用时为深色）；
  8 个排序项在最小窗口下仍能全部显示，只有未来项数增加才会出现滚动条。
- 设置页目前只有曲库分组；播放/外观/迷你/歌词分组仍待 P1 迁移。
- 输入框 hover 无独立样式（聚焦边框已足够）；原生 EDIT 的 IME 候选窗口位置
  由系统处理，未做自定义。
- 在线 Bamboo 仍未接入主程序；封面、原生 mini/桌面歌词已于 2026-11-10 接入
  （§10.1/§10.3）。
- 歌词面板的“编辑”按钮已接通独立编辑窗口（§10.2）；“独立窗口”见 §6.9。
- 独立窗口/overlay 的字体、颜色、行距等仍读取设置默认值，尚无设置页入口。

### 5.9 播放进度/时间不刷新（2026-11-09 修复）

- 现象：真实歌曲播放时，播放条时间与进度条停住；切换窗口、截图等整窗重绘时
  才跳到当前值。`--bench`（演示时钟）与 `--self-test` 正常。
- 根因：`App::tick` 用“`core.tick()` 前后各采一次位置”做差值。真实播放的位置由
  音频线程持续推进，两次采样只隔几微秒，`old_ratio ≈ new_ratio`：
  - 进度条边缘位移 < 0.05 DIP，`invalidate_progress_edge` 每帧跳过；
  - `floor()` 秒数永远相同，时间文本从不失效。
  演示时钟在 `core.tick()` 内部一次推进整个 tick 间隔，所以 bench 看不出来。
- 修复：新增 `App::last_position`，以**上一 tick 的位置**作为差值基准；tick 结束
  时（即使暂停/遮挡）都更新该基准。seek、切歌时重置基准，避免整条进度条误失效。
- 回归验证：30s 真实 WAV，7 秒内 `frames` 从 1 增至 59（修复前 150 次 tick 只有
  4 次绘制），进度条与时间按秒刷新。
- 约定：任何“按时间推进的动画”都必须用上一 tick 的基准值，禁止在同一 tick 内
  前后采样做 diff。

### 5.10 会话恢复：只记歌单位置 + mp3list（2026-11-09）

- **不再记忆每首歌的播放秒数**：`build_session` 固定写 `position_secs = 0.0`；
  启动时忽略旧 session 里的秒数（字段保留只为兼容旧文件）。重启后当前曲目
  从 0:00 开始。
- **只恢复歌单位置**：启动时先按 `current_key`（`Track::dedupe_key`）在队列中
  查找当前曲目，找不到再用 `current_index` 兜底；即使 `resume_on_start = false`，
  也会把该曲目设为 current 并停在 0:00（此前会清空 `queue.current`，看起来像
  “没记住播放到哪首”）。`resume_on_start` 现在只决定启动后是否自动播放。
- **mp3list 相关（对齐 ref `add_paths`/`import_playlist`）**：
  - 拖入或命令行传入 `.mp3list/.m3u/.m3u8` 时，解析歌单并**替换**当前队列；
    支持 `#SEARCH#<目录>[#type#*.ext]` 重新展开、`#BAMBOO#` 在线条目与直链。
  - 导入前记录当前曲目 key，导入后按 key 重新定位（新增/删除文件不会错位），
    新曲目从 0:00 开始；导入时若正在播放则继续播放，否则保持暂停。
  - `session.playlist_path` 与最近歌单记录会记住该文件；启动时若
    `playlist.auto_refresh_on_startup`（默认开）且文件仍存在，会重新展开歌单
    再按 key 恢复位置，保证目录里新增/删除的文件生效。
  - 空歌单/解析失败/目录已失效时**不覆盖**当前队列（返回 0）。
  - 单元测试覆盖：秒数不落盘、key 优先于 index、mp3list 导入后替换队列并记住
    文件、重启后重新展开且当前曲目不变。

### 5.11 点击跳转 / 点击穿透 / 歌曲右键菜单（2026-11-09）

- **点击穿透修复**：`App::on_click` 末尾的列表行点击现在要求
  `page == Queue || Library`。此前“正在播放”页点卡片会落到残留的列表矩形：
  行号等于当前曲 → 从头播放，相邻行 → 上/下一首，其他行 → 随机队列曲目；
  设置/搜索页的空白点击也会播放队列行，同样一并修复。
- **播放条封面/歌名左键**（`layout.cover` + `layout.track_text`）→ 切到
  “正在播放”页；已经在该页时不做任何动作。
- **右键菜单**（`WM_CONTEXTMENU`；播放条封面/歌名，或正在播放页的歌曲卡片）：
  - 复制歌名 / 复制歌手（为空时置灰）；
  - 打开文件位置（本地文件，`ShellExecuteW explorer.exe /select,...`）；
  - 打开 URL（`Url`/`Radio` 源，`webbrowser::open`）；
  - 复制歌曲 ID（Bamboo 源的 `song_id`）。
  - 使用原生 `TrackPopupMenu`（系统菜单样式，不另做自绘）；剪贴板用
    `CF_UNICODETEXT(13)` + `GlobalAlloc/GlobalLock`；弹出前
    `SetForegroundWindow`，关闭后补发 `WM_NULL`（系统菜单的常规收尾）。
  - 菜单数据在 `State` 借用结束后克隆出来再弹菜单，`TrackPopupMenu` 的模态
    消息循环不会重入借用。
- **验证**：用 portable 的 `target/verify` 构建加载 3 个 WAV：
  - 设置页点击播放条封面 → 正确回到“正在播放”；
  - 在正在播放页点击卡片封面 → 曲目与时间连续（00:13→00:14），不再换歌/重播；
  - 右键封面弹出菜单（复制歌名可用、复制歌手因无数据置灰、打开文件位置等），
    实机截图确认。
- `cargo test` 157 passed / 0 failed / 5 ignored；`cargo check` 0 warning。
  主 `target` 的 `cargo build` 因用户正在运行旧 exe 占用文件未替换，关闭后
  `cargo run` 即可。

### 5.12 歌词右键菜单（2026-11-09）

- 歌词子窗体处理 `WM_RBUTTONUP`，用 `hit_test` 求出光标所在行，向父窗口发
  `WM_APP_LYRIC_MENU`（`wparam = 行号 + 1`，0 表示不在任何行上）；独立歌词窗口
  会把该消息转发给主窗口，因此两个窗口都能右键。
- 主窗口构建原生 `TrackPopupMenu`：
  - 在此处播放：seek 到该行 `start_ms`（无时间轴时置灰）；
  - 复制文本：主行 + 翻译（第二行）；无文本置灰；
  - 复制时间点：`[mm:ss.xx]`（复用 `lyrics::editor::format_tag`），无时间置灰；
  - 分割线；
  - 重新加载歌词：`Command::ReloadLyrics`；
  - 导入歌词：rfd 选择 `.lrc/.srt/.txt` → `Command::LoadLyricsFile`；
  - 导出歌词：`LyricsEditor::from_lyrics(...).to_lrc()` 预先序列化，rfd 保存
    对话框默认名用原歌词文件名，否则 `<音频名>.lrc`；写入失败只记日志；
  - 打开歌词位置：仅本地曲目且 `Lyrics.path` 是实际文件时可用，Explorer
    `/select` 定位；内嵌/在线歌词置灰。
- 菜单数据（行文本、时间、LRC 导出文本）在 `State` 借用结束后克隆，模态菜单
  循环不会重入借用；导入/导出对话框在菜单返回后执行。
- 验证：带 `one.lrc` 的测试曲目在正在播放页右键当前行，全部项目可用；右键行间
  空隙/无歌词时前三项置灰，其余可用；用户实测通过。

### 5.13 队列/曲库右键菜单、队列页快捷按钮、曲库滚动修复（2026-11-09）

- **队列/曲库行右键菜单**（主窗口 `WM_CONTEXTMENU`，列表行优先于曲目信息菜单）：
  - 队列行：播放、从队列移除、复制歌名/歌手/文件路径、打开文件位置、打开 URL、
    复制歌曲 ID、清空列表（二次确认）。
  - 曲库行：播放、下一首播放（插到当前曲后）、添加到播放队列、复制歌名/歌手/
    文件路径、打开文件位置；文件缺失时播放类条目置灰。
  - 新命令：`Command::RemoveFromQueue(index)`、`Command::LibraryQueue { index, next }`；
    `Queue::insert_next()` 维护 current/history/shuffle_order/pending_cycle 索引；
    移除当前曲会停止播放并清空歌词，移除其他行只调整索引。
  - 菜单数据（标题/歌手/路径/URL/ID）在 `State` 借用结束后构建；命令在菜单关闭
    后执行。清空列表使用 `MessageBoxW(MB_YESNO)` 确认。
- **播放队列页右上角快捷按钮**：导入歌单 / 导出歌单 / 清空列表（`layout.queue_*`，
  `Target::QueueImport/QueueExport/QueueClear`）。
  - 导入：rfd 选择 `.mp3list/.m3u/.m3u8` → `Command::AddPaths`（按 ref 语义替换
    队列、重新展开 `#SEARCH#`、按 key 保持当前曲）。
  - 导出：rfd 保存，`core::playlist::mp3list::export(path, &queue, bom)`；默认名
    `playlist.mp3list`，遵循 `playlist.export_utf8_bom`。
  - 清空：确认后 `Command::ClearQueue`（同时清空歌词状态）。
  - 队列为空时导出/清空绘制为禁用态。
- **曲库虚拟列表滚动修复**：
  - 后台刷新（播放统计 `DbEvent::Changed`、索引进度）改为**保留已加载行数**
    （`start_library_query(keep_window=true)`），10k 曲库滚到深处后不会因只重载
    第一页而整屏空白、位置也不再跳。
  - 搜索、排序、进入曲库页调用 `reset_library_query()` 只取第一页；UI 同时
    `App::reset_list_scroll()` 把 scroll 归零。
  - `App::tick` 在 `library_rev` 变化时仍把 scroll 夹到新的 `max_scroll()`，作为
    行数收缩时的兜底。
- 单元测试：`insert_next` 顺序/去重、移除当前/非当前行后的 current 与播放状态；
  `cargo test` 159 passed / 0 failed / 5 ignored；`cargo check` 0 warning。

### 5.14 主窗口几何记忆 + 键盘快捷键 + 排序菜单 ESC（2026-11-09）

**主窗口位置/大小/最大化记忆**

- `AppCore::remember_main_window(pos, size, maximized)` 写入 `Session.window_*`
  （外框矩形，DIP）；bench 的 demo 时钟下直接返回，不写会话。
- 启动时在首次 `ShowWindow` 之前恢复：`SetWindowPos` 到记住的外框，再按
  `session.window_maximized` 用 `SW_SHOWMAXIMIZED` 显示，避免先闪一下默认尺寸。
- `WM_CLOSE` 保存（关闭到托盘与托盘“退出”都会经过）：`GetWindowPlacement`
  取 `rcNormalPosition`，最大化时也能记住还原尺寸；`IsZoomed` /
  `WPF_RESTORETOMAXIMIZED` 记录最大化状态。
- 记忆位置不在虚拟屏幕内（拔掉副屏/改分辨率）时只丢弃位置、保留尺寸，
  窗口不会跑到不可见区域。
- `State.persist` 在 `--bench` / `--self-test` 下为 false：不恢复、不写入。
- 单测 `main_window_placement_survives_restart` 覆盖 JSON 往返。

**键盘快捷键**

- Ctrl+←/→：上一首/下一首；Ctrl+↑/↓：音量 ±5%（暂停时也立即失效音量区，
  不依赖动画定时器）；Ctrl+O：打开音频文件选择器。
- “打开文件”对话框抽成 `pick_audio_files(hwnd)`，卡片按钮与 Ctrl+O 共用；
  菜单/对话框都在 `State` 借用结束后执行。
- 歌词子窗体把 `WM_KEYDOWN` 转发给父窗口，独立歌词窗口聚焦时快捷键同样生效。

**排序菜单 ESC**

- 曲库页排序 LISTBOX 打开时，ESC 优先关闭菜单（`sync_edit_boxes` 隐藏子窗体
  并立即重绘父窗口），不再落到“关闭播放设置/退出大歌词”。

**验证**

- `cargo build` 0 warning；`cargo test` 165 passed / 0 failed / 5 ignored；
  `--self-test` 与 `--self-test --page settings` 均 `painted=true`；
  `--bench 5`：`ticks=196, frames=212, avg_paint=525us, private=21.0MB ws=36.8MB`
  （内存本轮未优化，见 §5.15）。

### 5.15 曲库分页 + 列表行 tooltip（2026-11-10）

**曲库滚动改为整页查询**

- 现象：曲库滚到靠后位置后选中一首歌，后续曲目消失；像是动态流加载的窗口被重建。
- 根因：曲库是“先加载 2000 行 + 接近底部再追加”的流式虚拟列表。播放会写播放统计，
  `DbEvent::Changed` 触发后台重查；`scroll` 按旧行数计算，重查/重新排序后
  行数或顺序变化，滚动位置会落到新数据末尾之外，表现为后半段空白/消失。
- 修复：曲库改为**整页查询**，不再流式追加：
  - `page_size = floor(list.height / ROW_HEIGHT)`，每页只查询能完整显示的行数；
    `Command::LibraryPage { offset, limit }` 是一次完整窗口查询，
    `AppCore` 保存 `offset/limit`，后台刷新（播放统计、索引完成）重查**同一页**，
    不会截断当前列表。
  - 删除 `LibraryLoadMore` / `load_more_library` / `append` 与“接近底部自动加载”。
  - 超出末页（重新排序后总数变少）时，`AppCore` 回到最后一页重查；
    UI 在 `library_rev` 变化时把页码夹到新的页数。
  - 搜索/排序/进入曲库页回到第 1 页；窗口尺寸变化按新的每页行数重算页码，
    并保留大致相同的首行。
  - 底部固定分页条（参考 0.1.0 版）：`上一页` / `第 x / y 页` / `下一页`，
    右侧“每页 N 首”；禁用状态置灰且不进入命中测试。每页行数 = 列表区
    （已扣掉分页条高度）能完整显示的行数，窗口缩放后自动变化。
  - 曲库不再使用滚动条，滚轮在曲库页不滚动/不翻页；PageUp/PageDown、
    Home/End 仍可翻页。翻页时整窗失效重绘一次，避免旧行/旧分页条残留。
  - 曲库行改为两行：歌名 + 歌手（缺失时标注“文件缺失”）；右侧时长与
    “N 次”播放次数可同时显示（次数为 0 时不画）。当前播放曲目按路径高亮。
  - 点击/右键/上下文菜单仍使用页内行号；`AppCore.library.rows` 只包含当前页，
    因此行号天然对齐。
- 单测 `library_pages_load_only_the_visible_window`：5 首曲目查询 `offset=2,
  limit=2` 只返回 C/D；点击页内第 0 行播放 C，播放统计刷新后仍保持 2 行。

**播放统计不再覆盖曲库元数据（时长消失的根因）**

- 现象：播过的曲目只显示“N 次”，没播过的只显示时长，看起来像二者互斥。
- 根因：`record_play` 先 `upsert` 整行。播放态的 `DbTrack` 来自轻量 `Track`
  （`duration/bitrate/sample_rate` 为 `None`），upsert 把它们覆盖成 NULL；
  每播一次就抹掉一次索引里的时长。
- 修复：`record_play` 改为 `INSERT OR IGNORE`（仅当曲目未入索引时建行）+
  `UPDATE play_count/last_played_at`，播放统计不再改写任何扫描元数据。
- 单测 `record_play_keeps_indexed_metadata`：已有行 duration/bitrate 不被
  播放统计覆盖。
- 旧数据库里已被清空的时长需要“重新扫描”一次恢复。

### 5.16 进度条拖动改为“捕获 + 松手 seek”（2026-11-10）

- 现象：在进度条上下方的空白命中区（`progress_hit` 比 4 DIP 的条高 24 DIP）
  长按/拖动时，音频一直跟随光标 seek，解码反复重定位导致卡顿。
- 修复：
  - 只有真正的进度条（`progress.inflate(4)`）才进入拖动；上下留白只做悬停，
    不 seek。
  - 按下即 `SetCapture` 并进入“拖动预览”：只更新本地 `drag_preview`
    （进度填充、游标、时间文本从预览值绘制），**不向引擎发送 seek**。
  - `WM_LBUTTONUP` 时只发送一次 `SeekSecs`，然后清除预览；拖动过程不产生
    音频 seek，不会卡顿。
  - 独立歌词窗口的底部进度条同样改为捕获 + 松手 seek。
  - 进度边缘失效范围从 ±5 DIP 扩大到 ±12 DIP：按下的游标高 18 DIP，
    旧范围只覆盖条体附近，拖动时每个旧位置的游标上下会留下白色残影。
- 键盘左右 seek、歌词点击 seek 不受影响（仍是单次 seek）。
- 实机验证：拖动 25%→65% 松手，进度停在目标位置，播放条无白色游标残影。

**队列/曲库列表行 tooltip**

- 原生 `tooltips_class32`（`TTF_TRACK | TTF_ABSOLUTE`，450 ms 延迟，
  5 s 自动隐藏），不用自绘 tooltip，不参与主窗口脏区。
- 悬停行变化时更新文本并重新计时；`WM_MOUSELEAVE`/点击/滚轮/翻页/刷新后立即隐藏。
- 曲库行：歌名 / 歌手 · 专辑 / 格式 · 时长 · 播放次数（缺失时标注）/ 文件路径。
- 队列行：歌名 / 歌手 · 专辑 / 来源 · 无损 · 时长 / 路径或 URL（Bamboo 显示歌曲 ID）。
- 只在 `Target::Row` 悬停时显示；播放条、歌词、页头不弹。

**验证**

- `cargo build` 0 warning；`cargo test` 165 passed / 0 failed / 5 ignored；
  `--self-test --page now_playing|queue|library|settings` 全部 `painted=true`；
  `--bench 5`：`ticks=196, frames=228, avg_paint=562us, private=21.5MB ws=36.6MB`。
  内存相比上一轮上升（`AppCore::new` 的音频/初始化），本轮按用户要求暂不处理。

---

## 6. 正在播放重做规格（图 1，2026-11-09 定稿）

> 目标：主窗口“正在播放”按 0.1.0 egui 版截图重做；滚动歌词、独立歌词窗口、
> 歌词编辑窗口共用一套歌词视图逻辑，后续桌面歌词/迷你播放器也复用。
> 本轮先记录规格；已完成的小范围改动：`Command::ReloadLyrics` /
> `Command::LoadLyricsFile` 已加入 `AppCore`（编译通过）。

### 6.1 主窗口布局（对齐图 1）

自上而下：

1. **页头**：标题“正在播放”，副标题保留状态提示；右侧可显示格式徽标（如 `MP3`）。
2. **歌曲卡片**（圆角/描边可用矩形近似）：
   - 左侧封面（正方形，约 220 DIP；当前仍用占位音符，封面服务接入后替换）。
   - 右侧：
     - 标题（大号，当前曲目 `display_title`）。
     - 副标题：`歌手 · 专辑`（缺省不显示空 `·`）。
     - 徽标行：来源（本地/在线/Bamboo）、时长/格式。
     - 操作按钮：`播放/暂停`、`打开文件`、`桌面歌词`、`大歌词`。
3. **歌词面板**：
   - 头部：“歌词” + 来源标签（同名歌词文件/内嵌/在线/手动/已编辑/编码）。
   - 右侧按钮：`选择歌词文件`、`独立窗口`、`大歌词`、`编辑`、`刷新`。
   - 内容：**可滚动、可点击的完整歌词列表**（含翻译行）。
4. **底部播放条**：保持不变。
5. **Toast（可选，P2）**：右下角轻提示（队列恢复/索引完成等），需自动过期且
   不造成周期重绘。

窄窗口（最小 640×460 DIP）下按钮可缩短文案或分行；不得覆盖播放条。

### 6.2 歌词列表（核心组件）

**数据/逻辑（UI 无关，建议 `core::lyrics::view`）**

- 输入：当前 `Lyrics`、播放位置、当前行索引、翻译开关、行高参数。
- 输出：每行 top/height、总高度、当前行、当前行内 KTV 进度、命中测试
  （point → line index）、滚动目标（当前行居中）。
- 规则：
  - 有翻译行时每行占两行高度；翻译缺省时不留空。
  - 自动居中只在当前行变化时启动；用户滚轮/拖动后暂停自动居中约 2.5–3s。
  - 点击行 → seek 到该行 `start_ms`；点击后立即居中。
  - 当前行按播放进度做逐字/逐段填充（GDI 可用“整行画两遍 + 裁剪”近似）；
    翻译行不参与 KTV 填充。
  - 暂停/无时间轴/无歌词时不产生周期重绘。

**承载方式**

- 主窗口：独立子窗体（MFC 风格），拥有自己的滚动、hover、点击、滚动条、
  双缓冲绘制；父窗口只画卡片/面板边框和头部。
- 独立歌词窗口：新顶层窗口，内部复用同一个歌词子窗体组件。
- 桌面歌词/迷你播放器：复用同一 view 逻辑；桌面歌词按分层窗口 + 软件 D2D
  渲染整块歌词；迷你播放器可保留“紧凑单行”模式作为设置项。
- **禁止**把可滚动歌词列表直接画进主窗口脏区体系。

### 6.3 独立歌词窗口

> 状态：已实现（2026-11-09，§6.9）。窗口模式使用原生标题栏/边框（保留系统
> 最小化/最大化/关闭与主题色），F11 切换无边框全屏。

- 新顶层窗口（标准标题栏，可记忆尺寸/位置），内部仍是同一个歌词子窗体。
- 打开方式：歌词面板“独立窗口”按钮；再次点击或关闭窗口即销毁/隐藏。
- 与主播放器共用 `AppCore` 快照：显示当前曲目歌词、当前行、KTV 进度；
  点击歌词 seek 主播放器。
- 主窗口隐藏/最小化时仍可显示；由父线程按需同步，不新增独立音频状态。
- 关闭时必须从 `State` 清除句柄，避免悬挂窗口指针。

### 6.4 歌词编辑窗口（独立窗口）

- 打开方式：歌词面板“编辑”按钮；无歌词时禁用或从空文档开始。
- **第一阶段（P1）**：多行原生 `EDIT` 编辑 LRC 源文本 + `保存` / `取消`。
  - 初始内容来自当前 `Lyrics`（可经 `core::lyrics::editor::LyricsEditor` 生成 LRC）。
  - 保存到 `LyricsEditor::default_save_path()`（已有 sidecar → 覆盖；否则音频
    同目录 `.lrc`）；保存后触发 `Command::LoadLyricsFile` 重新加载。
  - 窗口内深色 EDIT（`WM_CTLCOLOREDIT`）、等宽字体、Ctrl+S 保存、Esc 取消。
- **第二阶段（P2）**：结构化编辑器（行列表、时间戳、翻译列、KTV 词级时间、
  撤销/重做、整体平移、按时间排序）。核心 `core::lyrics::editor` 已具备：
  `from_lyrics`、`to_lrc`、`write_to`、`undo/redo`、`stamp`、`add_after_selected`、
  `remove_selected`、`move_selected`、`sort_by_time`、`shift_all`、
  翻译导出；UI 只需补窗口与行编辑。
- 保存后歌词来源标记为 `Manual`/`Edited`，并在主窗口/独立窗口即时刷新。

### 6.5 操作与状态矩阵

| 控件 | 位置 | 行为 | 阶段 |
|---|---|---|---|
| 播放/暂停 | 卡片 | `Command::PlayPause` | P1 |
| 打开文件 | 卡片 | rfd 多选 → `Command::AddPaths` | P1 |
| 桌面歌词 | 卡片 | 启动/关闭原生分层桌面歌词 | P1（依赖 overlay 接回） |
| 大歌词 | 卡片/歌词头 | 主窗口隐藏卡片、歌词面板占满内容区；再点返回 | P1 |
| 选择歌词文件 | 歌词头 | rfd 单选 `.lrc/.srt/.txt` → `LoadLyricsFile` | P1 |
| 独立窗口 | 歌词头 | 打开/关闭独立歌词窗口 | P1 |
| 编辑 | 歌词头 | 打开歌词编辑窗口（6.4） | P1/P2 |
| 刷新 | 歌词头 | `Command::ReloadLyrics` | P1 |

按钮统一走 `src/ui.rs` 的 `Target`/`ButtonState`；未接功能的按钮置灰而不是假动作。

### 6.6 实施顺序（建议）

1. `core::lyrics::view`：行布局、当前行、滚动目标、命中测试（纯逻辑 + 单测）。
2. `src/lyrics_view.rs`：原生子窗体（双缓冲 GDI、滚动/悬停/点击/滚动条/KTV 裁剪）。
3. 主窗口布局改为图 1：卡片 + 歌词面板 + 子窗体；移除主窗口单行 KTV overlay。
4. ~~独立歌词窗口：复用步骤 2 的子窗体。~~（已完成，见 §6.9）
5. 歌词编辑窗口（第一阶段文本编辑）。
6. 桌面歌词/迷你播放器接入同一 view（桌面优先）；迷你保留紧凑单行开关。
7. 封面服务接入卡片；Toast（可选）。

### 6.7 验收

- 主窗口在 640 DIP 宽下卡片/歌词面板/按钮不重叠、不覆盖播放条。
- 歌词滚动仅重绘歌词子窗体；主窗口静态区域无周期重绘；暂停 GPU≈0。
- 点击任意歌词行 seek 正确，当前行自动居中，翻译行显示正确。
- 手动滚动 3s 后恢复自动居中；滚动条可拖拽。
- 独立窗口/编辑窗口关闭后主窗口与 `State` 无悬挂句柄。
- 关闭 KTV 或切换页面后不再有歌词条带残留。

### 6.8 实施记录（2026-11-09，步骤 1–3 完成）

**纯逻辑：`src/core/lyrics/view.rs`**

- `LyricsView`（`ViewConfig`：主行高 30 / 翻译行 20 / 间距 8 / 翻译开关）：
  `rebuild` 生成每行 `LineMetrics { top, height, text_height, translation_height }`；
  翻译为空或关闭时不占高度。
- `visible_range`（二分）、`hit_test`（跳过行间空隙，落点在行外返回 None）、
  `scroll_for_line` / `centered_scroll`（居中并 clamp）、`min_scroll` / `max_scroll`。
  滚动范围向内容两端各扩展半个视口，**首行/末行也能停在垂直中线**。
- `adjusted_position` / `line_end_ms` / `line_progress` / `current_line`：
  统一 offset、末行 4s 兜底，供主窗口子窗体与未来 overlay 复用。
- 7 个单测覆盖翻译行高、命中/空隙、首尾行居中、可见区间、offset 与无时间轴歌词。

**子窗体：`src/lyrics_view.rs`**

- 原生 `WS_CHILD` 控件，独立窗口过程与状态（`GWLP_USERDATA`），不借用父窗口
  `State`；父窗口只通过 `update(parent, &App)` 推送快照。
- 自有 GDI 持久双缓冲；`WM_PAINT` 只按 `ps.rcPaint` 清空/绘制/`BitBlt`：
  KTV 推进只失效当前行，滚动才失效整块。
- 子窗体自己处理 hover/pressed、滚轮（3 行/格）、滚动条拖拽（`SetCapture`）、
  点击行 → `PostMessage(WM_APP_LYRIC_SEEK, start_ms)`；歌词未计时时不跳转。
- 当前行：居中 + 强调色 KTV 填充（用 `karaoke_progress` 测量已唱前缀宽度并裁剪，
  支持逐字时间）；翻译行小字居中、不参与填充。
- ~~**长文本自动缩小字号**~~ **已取消（2026-11-10）**：固定字号；当前行超宽
  改为横向跟随（已唱边缘到右侧 7/8 才触发），其他行用省略号。见 §9.1/§9.2。
- 自动居中只在当前行变化时触发；手动滚动后暂停 3s，到期自动回中。
- 暂停时位置不变 → 不产生失效；父窗口被完全遮挡时跳过更新，恢复可见时
  `mark_dirty()` 整块重绘。
- 空状态显示“歌词加载中… / 暂无歌词 / 具体错误” + “拖放或选择 .lrc/.srt 文件”。
- **`WS_CLIPSIBLINGS` 必须设置**：否则兄弟窗口失效重绘时不会裁剪，歌词条带会
  画到播放设置弹窗上面（表现为“弹窗被歌词盖住”）。

**主窗口（`layout.rs` / `paint.rs` / `app.rs` / `main.rs`）**

- 布局改为 `compute_with(width, height, big_lyrics)`：歌曲卡片（封面/标题/
  歌手·专辑/来源·时长·格式徽标/播放·打开文件）+ 歌词面板（标题/来源 +
  选择歌词/独立窗口/大歌词/编辑/刷新 + 子窗体贴图区）。
- 窄窗口（640 DIP）自动压缩卡片高度、隐藏徽标行；最小窗口下卡片/面板/播放条
  不重叠。
- 大歌词模式隐藏卡片、歌词面板占满页面，按钮变为“返回”。
- 未接功能的按钮（编辑）统一置灰且不进入命中测试；独立窗口已接通（§6.9）。
- 移除单行 KTV overlay：`ktv_overlay.rs` 不再编译（保留作 D2D 参考）；
  `App::current_line` / `ktv_hint_rect` 删除，`tick` 只失效面板页头。
- 选择歌词文件走 `Command::LoadLyricsFile`，刷新走 `Command::ReloadLyrics`；
  “打开文件”用 rfd 多选音频文件 → `Command::AddPaths`。
- 顺带修复设置页扩展名输入框：高度 24→26 DIP、字体 15→13.5 DIP，避免下伸
  笔画被裁切。

**播放条重做 + 播放设置弹窗（2026-11-09 晚三）**

- 播放条高度 84→92 DIP，按参考图重排：
  - 左：封面 + 标题/歌手；
  - 中：随机、上一首、**播放/暂停（34×34，比左右 28×28 略大，同一行垂直居中，
    方形底色）**、下一首、循环；
  - 右：音量滑块（<820 DIP 宽时隐藏）+ 静音 + 播放设置 + 迷你播放器（置灰）+
    “词”（桌面歌词，置灰）+ 播放列表；
  - 底部：时间左/右 + 进度条（全宽）。
- 进度条新增 `Normal / Hover / Pressed` 三态：轨道变亮、填充变亮、条高 4→6→8 DIP，
  游标为细竖线（10/14/18 DIP），不再是白色大方块；进度/音量拖动使用 `SetCapture`。
- 音量滑块可点击/拖动；静音按钮切换；随机/循环按钮走 `Command::ToggleShuffle` /
  `CycleRepeat`（循环含单曲图标）；播放列表按钮切到“播放队列”页。
- **播放设置子面板** `src/player_panel.rs`：独立 `WS_CHILD` 子窗体，`HWND_TOP`
  置于歌词子窗体之上；内容包括播放速度（−/+，0.05 步进，0.5–2.0×）、均衡器开关、
  保持音高开关、打开设置页。子窗体自己画 hover/press，动作通过
  `WM_APP_PLAYER_ACTION` 发给父窗口转成 `Command`；点击面板外/ESC/切页/点歌词行
  自动关闭，关闭后歌词子窗体 `mark_dirty()` 补画被覆盖区域。
- 新增核心命令：`SetSpeed` / `SetEqEnabled` / `SetPitchPreserve`，修改后立即
  `engine.set_dsp(dsp_config(&settings))`（受 `features.speed_control/equalizer` 开关约束）。
- 修复：`--bench` 期间修改音量/速度/均衡器会被自动写回用户 `settings.json`；
  `maybe_autosave` 现在在 demo 模式直接返回，测试不再污染用户配置。
- 交互细节：歌词/弹窗子窗体点击时 `SetFocus(parent)`，空格/方向键仍作用于播放器；
  重叠子窗体必须设 `WS_CLIPSIBLINGS`，否则失效重绘会画到弹窗上。
- 主窗口类不再使用 `CS_DBLCLKS`：每次按下都是普通单击，快速连点“下一首/播放”
  不会被系统的双击时间窗吞掉（双击保护时间=0）。

**验证**

- `cargo build` 0 warning；`cargo test` 154 passed / 0 failed / 5 ignored；
  `--self-test --page now|queue|library|settings` 全部 `painted=true`。
- `--bench 8`：`ticks=314, frames=317, avg_paint=532us, private=11.1MB ws=27.4MB`
  （移除 D2D KTV overlay 后私有内存由 14MB 降到 11MB）。
- 真实歌曲 + 同名 LRC/`.zh.lrc` 桌面截图：翻译行、当前行居中高亮、KTV 填充、
  滚动条、点击行 seek（00:44 → 所点行 00:56）、大歌词切换均正常。
- 播放条截图验证：随机/循环/播放/下一首、音量 80% 填充、设置弹窗（速度 1.05×、
  均衡器/保持音高开关）、进度 hover 细游标、首行居中都正常。

### 6.9 独立歌词窗口（步骤 4，2026-11-09）

**窗口（`src/lyrics_window.rs`）**

- 独立顶层窗口，`WS_EX_APPWINDOW` 且**无 owner**：主窗口隐藏到托盘/最小化时
  仍然显示（有 owner 的窗口会随 owner 最小化而隐藏）。
- 窗口模式使用**原生边框/标题栏**（`WS_OVERLAPPEDWINDOW`，保留系统最小化/最大化/
  关闭和 Windows 主题色，不强制改色）；F11 / 窗体右上“全屏”图标切换无边框全屏
  （`WS_POPUP | WS_VISIBLE`，`HWND_TOPMOST` 覆盖当前显示器；ESC 退出）。
- 关闭按钮（原生 X 或壳内 X）隐藏窗口并向主窗口发 `WM_APP_LYRICS_CLOSED`；
  主窗口从 `State` 清除句柄、保存几何后 `DestroyWindow`，无悬挂指针。
- 尺寸/位置以 DIP 存入 `Session::lyrics_window_*`，同时记住窗口/全屏模式；
  启动时若上次开着则按原模式恢复（bench/self-test 不恢复、不写配置）。
- `WM_SIZE`/`WM_DPICHANGED`/模式切换只发 `WM_APP_LYRICS_SYNC`，主线程立即重排
  壳与歌词子窗体；`is_visible()` 把最小化视为不可见，最小化时不驱动动画定时器。

**沉浸式壳（`src/lyrics_screen.rs`）**

- 布局仿参考图（样式可调）：顶栏品牌 + 全屏/关闭；左列封面占位 + 标题/歌手/专辑；
  右列歌词；底栏时间/进度条 + 上一首/播放/下一首。
- 壳与主窗口各自独立 `Painter` 持久双缓冲；`WM_PAINT` 只清空/绘制/`BitBlt`
  `ps.rcPaint`：播放推进只失效进度边缘 + 秒数文本，当前行由歌词子窗体自身失效。
- 进度边缘失效按**像素差**判断（>0.05 DIP），不要用比例差：16ms tick 对 2:30
  的歌曲只推进约 0.0001 的比例，比例阈值（0.0005）永远跨不过，表现就是进度条
  不动、只有 hover 整条失效时才顺带刷新。
- 背景为近黑底 + 两个巨大的深绿椭圆；**歌词子窗体在浮动模式按自己在父窗口中的
  偏移重画同样的椭圆**，因此歌词区域与壳背景连续，不再出现纯色“黑块”。
- 点击进度条/拖动 → `WM_APP_LYRIC_SEEK`；上/下一首、播放暂停 →
  `WM_APP_LYRICS_ACTION`；点击歌词行仍由子窗体转发 seek。壳内顶栏在窗口模式下
  也可拖动窗口（发送 `WM_NCLBUTTONDOWN/HTCAPTION`）。
- 浮动歌词：行高 30→46 DIP、主行 15→20 DIP、翻译行 11.5→13.5 DIP，隐藏滚动条
  （滚轮仍可滚动），去掉当前行整行底色以贴近参考图，保留 KTV 强调色填充。

**接线（`src/app.rs` / `src/ui.rs` / `src/paint.rs` / `src/main.rs`）**

- 新增 `Target::LyricsWindow`；歌词头“独立窗口”按钮启用，悬停/按下状态由
  `App::lyrics_window_open` 驱动（打开时 Active）。
- 点击只置 `lyrics_window_requested`；主窗口在 `State` 借用结束后创建/销毁窗口
  （与 rfd 对话框同理，避免在借用中重入）。
- `sync_lyrics()` 统一同步内嵌面板与独立窗口；所有原有的 `lyrics.update` 调用点
  改为走它；`AppCore::remember_lyrics_window()` 写 session 并按需保存。
- 顺带：主窗口类设置 exe 内嵌图标（`LoadIconW` 资源 1），侧边栏左侧 4 DIP
  绿色装饰条已删除。

**验证**

- `cargo build` 0 warning；`cargo test` 154 passed / 0 failed / 5 ignored；
  `--self-test --page now_playing|queue|library|settings` 全部 `painted=true`。
- `--bench 300` 实机截图：窗口模式（原生绿色主题标题栏、最小化/最大化/关闭）、
  F11 全屏、ESC 返回；歌词区域与背景椭圆连续无黑块；点击“下一首”切到
  Track 00002、点进度 75% 跳到 01:54/02:31、播放/暂停图标与时间同步。
- 无鼠标输入下间隔 2.5s 截两次图：进度填充边缘 1137px → 1169px（168 DPI），
  证明播放推进只靠定时器自身失效即可连续更新。
- `--bench 8`：`ticks=318, frames=354, avg_paint=680us, private=11.2MB ws=27.6MB`。

---

## 7. 关键文件

```text
src/main.rs                     窗口/消息循环/定时器/拖放/托盘唤醒/--bench
src/app.rs                      UI 状态 + Command/Snapshot 适配 + hover/焦点/按区域失效
src/ui.rs                       可复用 UI 基础：Target/Input/ButtonState/Interaction
src/paint.rs                    GDI 绘制（虚拟列表/播放条/进度/时间/歌曲卡片/歌词面板）
src/lyrics_view.rs              滚动歌词子窗体（GDI 双缓冲、滚动/悬停/点击/滚动条/KTV）
src/lyrics_window.rs            独立歌词窗口（原生边框/无边框全屏、几何/模式记忆、消息转发）
src/lyrics_screen.rs            独立窗口沉浸式壳：布局/背景椭圆/封面信息/进度/播放控制
src/lyrics_editor_window.rs     独立歌词编辑窗口（原生多行 EDIT、Ctrl+S、保存后重载）
src/overlay_host.rs             桌面歌词/迷你播放器 host（状态映射、动作→Command、心跳）
src/player_panel.rs             播放设置弹窗子窗体（速度/均衡器/音量/overlay 开关）
src/ktv_overlay.rs              已停用：旧单行 KTV 分层子窗口（保留参考，不参与构建）
src/layout.rs                   布局矩形（含卡片/歌词面板/大歌词模式）
src/demo.rs                     --bench 夹具
src/core/app.rs                 AppCore / Command / Snapshot / 播放编排 / 封面
src/core/playlist/mod.rs        Queue（shuffle/repeat/dedupe/peek）
src/core/audio/mod.rs           AudioEngine（cpal/symphonia/gapless/DSP）
src/core/lyrics/service.rs      歌词后台加载
src/core/lyrics/view.rs         可复用滚动歌词视图：行布局/当前行/偏移/命中
src/core/util/cover.rs          当前曲目封面 + 行缩略图后台服务
src/core/config/mod.rs          Settings / Session / Paths / 原子 JSON
src/core/platform/tray.rs       托盘（无 egui 依赖）
src/core/platform/native_overlay.rs / native_mini.rs
src/core/desktop_overlay/       原生桌面歌词窗口（已接入，§10.3）
src/core/mini_overlay/          原生迷你播放器窗口（已接入，§10.3）
ref/archived/bamboo-egui/       0.1.0 egui 版历史交接与实现参考
```

---

## 8. 与 0.1.0 egui 版的功能差距（2026-11-09 历史快照，最新状态见 §26–§27）

> 对照 `ref/archived/bamboo-egui/README.md` 与 `HANDOFF.md` 整理。归档版只作功能
> 参考，不参与构建。业务核心大多已迁入 `src/core/`，因此差距分三类：
>
> - ✅ 已在新版可用
> - 🟡 业务核心已在 `src/core/`，但 UI / Command / 设置尚未接线
> - ❌ 功能缺失（核心也缺或只有配置字段）
> - ➖ 明确不做 / 暂不考虑

### 8.1 在线音源、搜索与发现

- 🟡 Bamboo API 客户端：搜索/建议/歌曲/专辑/歌手/info/resolve、多音源、
  Cookie/请求头、代理/TLS/超时/重试（`src/core/api/` 已迁移）。
- ❌ Bamboo 在线曲目播放：`AppCore` 的 `TrackSource::Bamboo` 分支仍只写
  “在线音源尚未接入”日志；需要解析播放地址、流缓存与预解析下一首。
- 🟡 搜索缓存（音源+类型+关键词/ID+页码，最多 80 页/7 天，离线回退与
  “缓存结果”徽标）：`core/api/cache.rs` 已有。
- ❌ 搜索页 UI 与搜索建议防抖；当前“搜索”页只是占位。
- ❌ 专辑/歌手详情页；在线封面/缩略图（仅为可见行加载）。
- ❌ 在线歌词（`info` 返回的 LRC）、下一首临近曲尾预解析、快速连续切歌时
  在线歌词兜底。
- ❌ 分享链接/直链输入入口（Bamboo 分享链接、http(s)/HLS URL）；mp3list 内的
  URL 已能生成 `TrackSource::Url`，但没有“打开 URL”对话框。
- 🟡 发现 / 随机电台：`core/radio` 已有；缺发现页、自动补货、最近 8 条电台
  历史、随机漫游 18 首。
- 🟡 智能歌单：`library/db.rs` 已有 `SmartKind` 查询；缺入口与页面。
- ❌ 在线设置 UI：API 请求最小间隔（0–3000ms）、专辑/歌手每页数量、代理、
  TLS、超时/重试等（配置字段已存在）。

### 8.2 下载、流缓存与封面

- 🟡 后台下载队列：并发调整、文件名模板、同名策略、标签/封面/歌词写入、
  HTTP Range 断点续传、HLS 点播下载、单曲音质选择（`core/download` 已有）。
- ❌ 下载管理页、队列右键“下载”、失败重试、清理未完成 `.part`。
- 🟡 流缓存/HLS 缓存/LRU 清理/占用统计、离线模式、缓存命中后的 TTL 后台刷新
  （`audio/http_source` 与设置字段已有；`AppCore` 目前给 URL 传 `cache: None`）。
- ✅ 真实封面：播放条、歌曲卡片、队列/曲库列表行、独立歌词窗口、桌面歌词/
  迷你播放器、分享图与 SMTC PNG 均由 `CoverService`（当前曲目）+
  `RowCoverService`（可见行 64px 缩略图）提供；无封面回退占位图标（§10.1）。
- ❌ 缓存管理 UI：封面/歌词/流/响度/指纹占用与一键清理（不删除下载文件）。

### 8.3 播放与音频增强

- 🟡 无缝播放/交叉淡化：引擎与设置已支持（`audio/mod.rs` 双 deck、
  `playback.crossfade_secs/gapless`）；缺设置 UI（当前交叉淡化固定 0s）。
- 🟡 10 段均衡器：DSP 已消费 `eq_bands/eq_preset`；播放设置弹窗只有总开关，
  缺 10 段推子与预设编辑器。
- 🟡 ReplayGain：DSP 已消费模式/预增益，`audio/loudness` 有缓存增益查询；
  缺模式/预增益 UI 与无标签文件的 EBU R128 后台扫描接线。
- ❌ A-B 循环：只有 `features.ab_repeat` 开关，缺区间逻辑与 UI。
- ❌ 睡眠定时：只有 `features.sleep_timer` 开关，缺倒计时逻辑与 UI。
- 🟡 播放统计：播放/跳过已写库（`record_play/record_skip`）；缺统计页
  （总时长、播放次数、最近播放、歌手榜）。
- 🟡 播放设备选择：`playback.device` 字段存在，未接 UI/引擎。
- ✅ 变速 0.5–2.0（`SetSpeed` clamp）、保持音高、均衡器开关、随机、循环
  （含单曲）、自动下一首、音量/静音。
- ✅ 会话恢复只记歌单位置（按 `current_key`，从 0:00 开始）；这是与 0.1.0
  “记住单曲秒数”的有意差异（用户 2026-11-09 决定）。

### 8.4 队列、歌单与文件夹浏览

- ✅ 队列排序：队列行右键上移/下移/置顶/置底（`Queue::move_item`，筛选时置灰）。
- ✅ 队列筛选：按歌名/歌手/专辑/来源即时过滤，原生 EDIT + 显示行映射（§11.3）。
- ➖ “我的歌单”侧栏：用户 2026-11-10 明确不要；`Session.playlists` 数据保留
  供后续文件关联使用，不显示。
- 🟡 文件夹浏览页：`library/browser.rs` 已有；缺 UI（下一批）。
- ✅ `.mp3list` 文件关联：设置页“系统”分类开关，启动时同步注册表（§11.2）。
- ✅ 队列页导入/导出/清空、行右键（播放/移除/清空）、mp3list `#SEARCH#`
  重新展开、拖放/命令行打开歌单、启动按 key 恢复歌单位置。

### 8.5 曲库

- 🟡 统计与智能歌单：`library/db.rs` 已有统计查询与 `SmartKind`；缺页面/命令。
- 🟡 查重结果页：db 支持“标题+歌手+时长 / 同一路径 / 音频指纹”分组；设置页
  目前只有查重方式，缺重复组列表、合并、移除索引（不删文件）。
- ✅ 缺失文件检查/清理入口：曲库工具栏“检查缺失”调用
  `LibraryDb::check_missing`，结果写入页头副标题；行右键“从曲库移除”只删索引
  （二次确认，不删文件）（§17）。
- ✅ 索引取消：扫描中“重新扫描”按钮变为“取消索引”，调用
  `LibraryDb::cancel_index`，在文件之间停止并回写已扫描结果（§17）。
- ✅ “全部入队”：按当前搜索/排序在 DB 线程查询整库（`QueryAll`），
  不再只入队当前页（§17）。
- ✅ SQLite 索引、搜索/排序/整页分页、目录增删、扩展名、启动扫描、
  目录监听、符号链接、8 种排序、10k 曲库（每页只绘制当前页行）。
- ✅ 后台刷新重查同一页窗口、搜索/排序/进页回第 1 页、末页夹取；
  选中靠后曲目后不再出现后续行消失。
- ✅ 队列/曲库行悬停 tooltip（歌名/歌手/专辑/时长/路径，原生 tooltip）。

### 8.6 歌词

- ✅ 歌词编辑器：独立窗口（原生多行 EDIT + 保存/取消 + Ctrl+S/Esc）已接入
  歌词面板“编辑”与 Ctrl+E；保存后重新加载为 `LoadLyricsFile`（§10.2）。
- ✅ 桌面歌词原生窗口：`core/desktop_overlay` 已由 `overlay_host` 启动并同步；
  右键菜单的锁定/穿透/置顶/字号/行数/对齐/尺寸/重置位置全部转成 `Command`，
  位置/尺寸持久化（§10.3）。
- ✅ 迷你播放器原生窗口：已接线；覆盖层动作（播放/切歌/静音/音量/seek/几何）
  与主窗口共用 `Command`；暂停后的图标刷新问题已修复（§10.3）。
- ➖ 大歌词页全屏与指定显示器：暂不考虑（用户 2026-11-09 决定）；当前
  `big_lyrics` 只是主窗口内布局。
- ❌ 独立歌词窗口的字体/字号/行距/颜色、译文/逐字开关、封面/曲目信息/进度条
  显示设置（当前壳内为硬编码）。
- ✅ 歌词分享图 1200×1600：歌词右键菜单“生成分享图…”（后台 GDI+ 渲染，§10.2）。
- ❌ 歌词编码/偏移/翻译语言/逐字显示/预取下一首本地歌词的设置入口
  （`settings.lyrics` 字段已有；`LyricsService::request_prefetch` 未被调用）。
- ✅ “编辑 / 桌面歌词 / 迷你播放器 / 分享图”入口均已接通。
- ✅ LRC/SRT、编码自动、逐字 KTV、当前行居中、翻译 sidecar、点击/右键菜单、
  独立窗口（原生边框 + F11 无边框全屏）。
- ✅ 固定歌词字号（不再按行自动缩小）；当前行超宽时横向跟随，已唱部分到达
  右侧 7/8 即开始左移并保留 1/8 留白（§9.1/§9.2）。
- ➖ 右侧歌词栏 / 边栏歌词（用户明确不做）。

### 8.7 桌面与系统集成

- ✅ 单实例 + 第二实例参数转发 + 多实例设置：第二实例把命令行写入命名管道后
  退出，第一实例在 `WM_APP_WAKE` 中处理路径与跳转列表动作（§11.1）。
- ✅ 系统媒体控制 SMTC（封面/时间轴/拖动 seek）：已接主窗口，按钮事件映射
  `Command`（§11.1）。
- ✅ 系统通知（切歌 Toast）：功能开关 + 非前台条件（§11.1）。
- ✅ 开机启动、音频文件关联、`.mp3list` 关联：设置页开关 + 启动/变更时同步
  注册表；便携模式跳过（§11.1/§11.2）。
- ✅ 任务栏跳转列表（快捷任务 + 最近 8 个本地文件）：最近播放变化时重建
  （§11.1）。
- ➖ 命令面板（Ctrl+K）：暂不考虑（用户 2026-11-09 决定）。
- ✅ 主窗口位置/大小/最大化记忆：启动时在首次显示前恢复，`WM_CLOSE`
  （含隐藏到托盘）保存；离屏位置丢弃、尺寸保留；bench/self-test 不写会话。
- ✅ 关闭行为/启动最小化设置：设置页“窗口”分类（§11.2）。
- ✅ 主窗口与独立歌词窗口的录屏捕获保护：`SetWindowDisplayAffinity`，切换立即
  生效。
- ❌ 应用内 Toast/状态提示（错误目前只写日志或显示歌词错误文本）。
- ✅ 托盘：显示/隐藏、播放/上一首/下一首、桌面歌词、锁定、随机、循环、
  音量、退出；桌面歌词/锁定已与设置联动。

### 8.8 外观、性能与无障碍

- ❌ 字体/字号/颜色的逐面选择（主题调色板与强调色已完成；`paint.rs`
  字体族/字号尚未接 `theme.font_*`）。
- ➖ 界面缩放 75%–160%：已于 §27 移除（缩放后布局/缓冲错位）；有效 DPI 只使用系统 DPI。历史实现：
  鼠标坐标统一缩放；窗口自动放大到 640×460 DIP 最小逻辑尺寸（§18.2）。
- ✅ 动画/界面帧率上限 15–120：`performance.frame_rate` 驱动动画定时器
  （`frame_millis()`），修改后立即重启定时器（§18.2）。
- ❌ 背景：模糊封面/流动渐变/Mica（归档版用 wgpu；原生需 D2D/DWM 方案可选）。
- ❌ 音频可视化。
- ❌ 无障碍：UI Automation/AccessKit、Tab 焦点、滑杆方向键/Home/End。
- ✅ 播放/窗口/系统设置分组：无缝/交叉淡化/ReplayGain/EQ 预设/睡眠定时、
  关闭行为/捕获保护、单实例/开机启动/关联/SMTC/通知/跳转列表（§11.2）。
- 🟡 快捷键：Ctrl+←/→（上/下一首）、Ctrl+↑/↓（音量）、Ctrl+O（打开文件）、
  Ctrl+E（歌词编辑器）已支持；基础快捷键：Space、左右 seek、
  上下/Home/End/PgUp/PgDn。F11 大歌词/全屏与 Ctrl+K 命令面板暂不考虑。
- 🟡 性能设置：内存回收、索引优化等字段已有，UI 未接。
- ✅ 中英资源文件（`src/core/i18n/en.json`）、DPI 适配、最小窗口、局部重绘。
- ➖ 中英双语切换：暂不考虑（用户 2026-11-09 决定），后续再做语言设置与文案清理。

### 8.9 明确不做 / 不适用

- ➖ 主窗口重新引入 egui/eframe/wgpu/WebView；双 UI 并行维护或对照。
- ➖ 右侧歌词栏 / 边栏歌词。
- ➖ egui 的渲染后端与显卡偏好设置（DX12/Vulkan/GL）：原生 UI 不使用 wgpu。
- ➖ 用降帧率/降画质换 GPU 占用（见 AGENTS）。
- ➖ 大歌词独立全屏页/显示器选择、命令面板（用户 2026-11-09 决定暂不考虑）。
- ➖ 圆角细化（卡片/歌词面板/按钮保持当前的简化矩形外观；窗口自带的 DWM
  圆角/阴影仍由系统处理，不作为待做项）。
- ➖ 多音源、请求头、端点覆盖等音源高级设置 UI：只保留一个在线 API 地址。
- ➖ 中英双语切换（暂缓，见 8.8）。

### 8.10 建议补齐顺序

**P0（日常体验）**

1. ✅ 真实封面服务（播放条/卡片/列表/独立窗口/overlay），见 §10.1。
2. ✅ 歌词编辑器窗口 + 分享图，见 §10.2。
3. ✅ 桌面歌词、迷你播放器接线，见 §10.3。
4. ✅ 关闭行为设置 + 单实例 IPC + 第二实例转发（§11.1）。
5. ✅ 捕获保护、SMTC、通知、跳转列表、开机启动/文件关联（§11.1）。

**P1（核心功能）**

6. 播放设置页：无缝/交叉淡化、10 段 EQ/预设、ReplayGain、A-B、睡眠定时。
7. 文件夹浏览页（队列排序/筛选已完成；我的歌单侧栏用户明确不做）。
8. 曲库统计/智能歌单/查重页；缺失检查、索引取消、全部入队已完成（§17）。
9. 搜索页、Bamboo 在线播放与在线歌词、发现/随机电台。
10. 下载管理、流缓存/离线模式、缓存清理。

**P2（可选）**

11. 外观/无障碍/快捷键补全（界面缩放与帧率上限已完成，见 §18.2；
    双语、命令面板暂缓）。
12. 模糊背景、可视化等视觉增强。

> 在线音源、下载与搜索缓存依赖可用的 Bamboo 站点；建议先把 `core/api` 的
> 播放/歌词/预解析接进 `AppCore`，再补 UI。所有优化仍需遵守 AGENTS 的
> 局部重绘与性能验收约束。

---

## 9. 歌词字号/横向滚动 + 窄屏齿轮面板（2026-11-10 已实施）

### 9.1 歌词字号固定（不再自动缩小）

- `src/lyrics_view.rs` 删除了“0.7 DIP 步进缩小直到放得下”的循环和 `min_size`：
  内嵌当前行/普通行保持 15/14 DIP，浮动/独立窗口保持 20/18 DIP；超宽不再改字号。
- 字号以后可直接接 `Settings.lyrics.font_size`（默认 17.0，字段已有），
  本轮先保持原常量，避免和现有布局高度联动。
- 超宽处理：当前行横向跟随（§9.2）；非当前行仍用 `DT_END_ELLIPSIS` 裁剪。

### 9.2 当前行横向跟随：到右侧 7/8 开始滚动

- 新增纯函数 `horizontal_follow(text_width, sung_width, available, current)`：
  - 仅“当前行 + 文本宽度 > 可用宽度”时生效；
  - 已唱边缘到达 `available * 7/8` 时开始把文字左移，保持右侧 1/8 留白，
    而不是等到文字已经贴边/被裁掉才动；
  - 偏移上限 `text_width - available + available/8`，行尾同样保留 1/8 空白。
- 绘制时文字与 KTV 填充共用同一个偏移，并统一裁剪到歌词行内；
  非当前行不滚动，翻译行保持居中/静态。
- 失效仍走既有路径：KTV 进度变化只失效当前行；暂停/短行不产生周期重绘。
- 3 个单测：7/8 触发阈值、短行/非当前行不动、行尾保留 1/8。
- 实机（portable 87 字长行，10s）：2s 时从左显示，8s 时已左移且已唱部分保持在
  右侧约 1/8 内，字号保持不变。

### 9.3 窄屏齿轮面板补充隐藏功能

- 播放条宽度 <900 DIP 隐藏桌面歌词/迷你播放器按钮，<820 DIP 隐藏音量滑块；
  这些功能改由齿轮面板提供。
- `src/player_panel.rs` + `layout.rs`：
  - 面板 236×176 → 260 宽；高 208（宽屏）/264（窄屏）。
  - 新增**音量行**：0–100% 滑块 + 百分比（静音时显示“静音”）；拖动用
    `SetCapture`，只发 `ACTION_SET_VOLUME`（lparam = volume × 1000），父窗口转成
    `Command::SetVolume`；与播放条音量/静音双向同步。
  - 窄屏时面板底部增加“桌面歌词 / 迷你播放器”两行；2026-11-10 起为真实开关
    （开/关底色与播放条同步），宽屏不显示，避免与播放条重复。
- 验证：726 DIP 窄窗口打开齿轮面板，音量 80%、速度、EQ、保持音高、设置页入口
  与两行 overlay 开关完整显示；发送 `ACTION_SET_VOLUME=250` 后播放条音量填充约 25%。

**验证**

- `cargo build` 0 warning；`cargo test` 165 passed / 0 failed / 5 ignored；
  `--self-test --page now_playing|library` 均 `painted=true`。
- 歌词横向跟随与齿轮面板已用 portable 窗口实机截图确认（PrintWindow，175% DPI）。

---

## 10. 三批功能落地（2026-11-10）

### 10.1 批次 1：真实封面服务

**核心（`core/app.rs` + `core/util/cover.rs`）**

- `AppCore` 新增 `CoverState`：当前曲目封面经 `CoverService` 在后台线程解码，
  本地标签/同目录 cover、在线 `cover_url` 都走同一路径；结果带递增
  `cover_rev`，UI 只读快照，不在绘制线程解码。
- 新增 `RowCoverService`（独立 worker，与当前封面互不取消）：按
  `normalize_path` 缓存 64px 缩略图，最多 256 项；`None` 也会缓存，避免
  无封面的文件被反复读取。队列/曲库只请求当前可见行。
- 封面服务/行缩略图完成后通过既有 `wake` 回调 `PostMessage(WM_APP_WAKE)`；
  暂停时也能立刻换图。
- 切换曲目时先清空上一张并 bump `cover_rev`，解码完成后再次 bump；旧请求用
  generation 丢弃。`performance.load_covers=false` 时完全不发请求。

**UI**

- `Painter::cover`：RGBA→BGRA 转换按图片地址缓存，`StretchDIBits` + HALFTONE
  居中裁成方形；缓存上限 384 张，避免每帧转换。
- 播放条封面、正在播放歌曲卡片、独立歌词窗口左列都绘制真实封面，失败时
  回退占位音符。
- 队列/曲库行左侧增加 30×30 缩略图，标题/歌手右移；仍只绘制可见行。
- `Snapshot` 增加 `current_cover/current_cover_png/cover_rev/row_cover_rev`；
  行缩略图到达只失效列表区，当前封面到达只失效播放条/卡片封面。
- 实机（portable WAV + 同目录 `cover.png`）：播放条、卡片、迷你播放器均显示
  真实封面；列表行缩略图正常，无封面文件保留占位。

### 10.2 批次 2：歌词编辑窗口 + 分享图

**编辑窗口（`src/lyrics_editor_window.rs`）**

- 独立顶层窗口（owner = 主窗口），原生多行 `EDIT` + “保存/取消”按钮；
  深色背景、透明边框输入、`DarkMode_Explorer` 滚动条。
- 初始内容来自 `LyricsEditor::from_lyrics(...).to_lrc()`；默认保存路径沿用
  核心的 `default_save_path()`（已有 sidecar 覆盖，否则音频同目录 `.lrc`）；
  没有路径时打开“保存为”对话框。
- 保存后把 `PathBuf` 通过 `WM_APP_EDITOR_SAVED` 交给主窗口，再走
  `Command::LoadLyricsFile` 重新加载；`WM_APP_EDITOR_CLOSED` 清句柄。
- 关闭时若有未保存修改，`MessageBoxW(MB_YESNO)` 确认；`Ctrl+S` / `Esc`
  通过子类化 EDIT 转发（焦点在输入框时也能用）。
- 歌词面板“编辑”按钮与 `Ctrl+E` 已接通，不再置灰。

**分享图**

- 歌词右键菜单新增“生成分享图…”：取当前行前后共 5 行（含翻译），
  标题/歌手·专辑、当前行高亮、当前封面 PNG、主题强调色。
- 保存对话框（默认 `<歌名>-歌词分享.png`）关闭后，GDI+ 渲染 1200×1600 与
  PNG 编码在后台线程执行，不阻塞 UI。

### 10.3 批次 3：桌面歌词 / 迷你播放器接入

**Host（`src/overlay_host.rs`）**

- `DesktopHost` / `MiniHost` 各自持有 overlay 线程的收发通道；`sync` 把
  `Snapshot` + `DesktopLyricsSettings`/`MiniPlayerSettings` 映射成
  `OverlayState`/`MiniState`，歌词/封面只在版本变化时附带，位置心跳 250ms，
  全量状态 500ms。
- 播放状态变化（播放↔暂停）**立即**发全量状态：暂停会停掉主窗口 16ms 定时器，
  若仍等 250ms 心跳，overlay 会一直显示旧的暂停/播放图标，直到拖动窗口触发
  几何动作才刷新（用户报告的 bug）。`state_due()` + 单测锁定该条件。
- 处理 overlay 动作时先转成 `Command`（播放/上一首/下一首/seek/音量/静音、
  锁定/穿透/置顶/字号/行数/对齐/自动宽度/尺寸/几何位置），随后 `force_state()`
  确保暂停状态下这些变化也能立即回显。
- 几何位置写入 `Settings.desktop_lyrics.position` / `Session.desktop_lyrics_pos`
  与 `Settings.mini.position`，退出时随 settings/session 保存。

**主程序接线（`main.rs`）**

- `sync_overlays` 按设置启停线程：桌面歌词 = `enabled && visible_at_start`，
  迷你播放器 = `mini.visible`；启动失败时自动关闭对应设置，避免每帧重试。
- 播放条“词”/迷你图标、齿轮面板窄屏两行均为真实开关，Active 状态与设置同步。
- 托盘“桌面歌词/锁定”不再只是占位状态，分别映射
  `ToggleDesktopLyrics` / `DesktopLyricsSetLock`。
- 定时器在主窗口隐藏/最小化但 overlay 打开时继续跑位置心跳；overlay 自身
  60fps 插值，暂停且无 overlay 时不产生周期重绘。
- `core/desktop_overlay` 线程补 `SetThreadDpiAwarenessContext(PER_MONITOR_AWARE_V2)`：
  此前 175% 下窗口按 96 DPI 创建（半尺寸/巨大文字），现在与系统 DPI 一致。

**验证**

- `cargo build` 0 warning；`cargo test` 169 passed / 0 failed / 5 ignored；
- `--self-test --page now_playing|queue|library|settings` 全部 `painted=true`；
- `--bench 5`：`ticks=201, frames=202, avg_paint=472us, private=22.6MB ws=40.0MB`；
- portable 实机（175% DPI，真实 WAV + cover.png）：
  - 桌面歌词窗口 1330×350 @ (140,140)，迷你播放器 665×301 @ (140,665)，
    均按 175% 正确缩放；封面与进度显示正常。
  - 点击迷你播放器暂停按钮后 1s 内截图，图标已从暂停条切换为播放三角，
    不再需要移动窗口；恢复播放同样正常。

### 10.4 透明窗口不再算作遮挡（2026-11-10）

- 现象：桌面歌词/迷你播放器等透明窗口盖住主窗口时，主窗口在该区域不重绘；
  播放进度、歌词或卡片变化后，透明区域下方会留下旧像素。
- 根因：`visible_rects` 按 z-order 枚举上方窗口，把所有可见窗口都当成不透明
  遮挡物从客户区减去。分层窗口（`WS_EX_LAYERED`）可能是任意 alpha，
  `WS_EX_TRANSPARENT` 更是点击穿透；它们的矩形不能代表“完全不可见”。
- 修复：枚举时跳过 `WS_EX_LAYERED | WS_EX_TRANSPARENT` 窗口，主窗口继续在
  这些区域重绘。代价只是叠在透明窗口下的一小块动画脏区被多画一次；页面切换
  的整窗重绘也只在切换时发生，不需要设置开关。若以后出现大面积全屏分层窗口，
  再考虑加“把分层窗口视为遮挡（省电）”的可选项。
- 相关约定已写入 `AGENTS.md`；`docs/NATIVE_UI_EXPERIENCE.md` §2.14 记录原因。

---

## 11. 第四至六批（2026-11-10）

### 11.1 系统集成

**单实例与第二实例转发**

- 启动阶段（创建窗口之前）读取 settings：`allow_multiple_instances=false` 时用
  `platform::single_instance` 的命名互斥量判断；已在运行则把命令行通过
  `platform::ipc` 命名管道转发给第一个实例，自身退出码 0。
- `--bench` / `--self-test` 不参与单实例；便携测试可用 `BAMBOO_INSTANCE` 隔离。
- 第一个实例启动 `ipc::start_server()`；`WM_APP_WAKE` 里 drain 转发参数：
  已有路径 → `AddPaths`（文件播放/歌单替换队列），并支持
  `--play-pause`、`--next`、`--open-files`、`--import-playlist`。
- 跳转列表任务同步去掉未实现的“打开链接…”，其余任务可直接工作。

**SMTC / 通知 / 跳转列表**

- `SmtcController` 接到主窗口：每 tick 更新标题/歌手/专辑/播放态、封面 PNG
  （`set_thumbnail`）与 250ms 节流的时间轴；按钮事件映射
  Play/Pause/Next/Prev/Stop/Seek → 既有 `Command`。
- 切歌 toast：仅当功能开关打开、非 bench/self-test、主窗口不在前台时发送；
  启动恢复的第一首不弹。
- 最近播放：`AppCore::remember_recent()` 在本地曲目真正加载时写入
  `Session.recent_files`（去重、最多 8 首）；`jump_list::update` 在该列表
  变化时重建任务栏跳转列表。便携模式不写系统跳转列表。

**窗口行为 / 注册表**

- `WM_CLOSE` 按 `window.close_behavior`：托盘 / 直接退出 / 询问（否=托盘）。
  托盘“退出”仍绕过该设置。
- 最小化回到任务栏（`WM_SYSCOMMAND/SC_MINIMIZE` 不再拦截）。
- `start_minimized` 在首次 ShowWindow 时使用 `SW_SHOWMINIMIZED`，任务栏按钮
  仍可用（不再隐藏到托盘；托盘仍创建 overlay/图标）。
- `capture_protection` 调用 `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`；
  独立歌词窗口使用 `lyrics_capture_protection`，切换立即生效。
- 开机启动、音频文件关联、`.mp3list` 关联按 settings 与注册表实际状态同步；
  便携模式完全跳过注册表写入。

**暂停时保存设置**

- 暂停窗口没有动画定时器，原 `maybe_autosave` 不会触发。新增
  `AppCore::flush_settings()`：设置页切换、曲库开关、播放设置弹窗、overlay
  动作与托盘动作后立即落盘；拖动音量仍走节流保存，避免每个鼠标移动写文件。

### 11.2 设置页：曲库 / 播放 / 窗口 / 系统

- 设置页顶部新增四个分类按钮（`SettingsTab`），内容区从 `header.bottom+46`
  开始；曲库页原有布局整体下移，功能不变。
- **播放**：无缝播放、自动下一首、交叉淡化（±1s，0–30s）、ReplayGain
  （关/曲目/专辑）、预增益（±1dB）、均衡器开关、7 个预设、睡眠定时
  （关/15/30/60 分钟）。
- **窗口**：关闭行为（托盘/退出/询问）、启动时最小化、
  记住窗口位置、录屏捕获保护、歌词窗口捕获保护。
- **系统**：允许多实例（需重启）、开机启动、音频文件关联、`.mp3list` 关联、
  系统媒体控制、切歌通知、任务栏跳转列表。
- 通用行布局由 `layout::setting_row/control/segments/step_rects` 计算；
  paint 与命中测试共用，hover/pressed/active 复用既有 `Interaction`。
- 睡眠定时在 `AppCore` 内倒计时；到期自动暂停并清空选择。切换开关立即
  `flush_settings`，不需要等到下一 tick。
- 切换分类时隐藏曲库页的原生扩展名 EDIT（只在该分类显示）。

### 11.3 队列筛选/排序

- **队列筛选**：队列页头部复用原生 `EDIT`（“筛选队列…”），按歌名/歌手/
  专辑/来源即时过滤。`App.queue_view` 保存“显示行 → 队列真实下标”的映射；
  行点击、右键菜单、封面缩略图、tooltip、当前行高亮全部先映射再访问队列。
- 队列内容变化由 `Snapshot.queue_rev` 驱动重建映射；筛选变化立即重建并
  重置滚动，页头副标题显示“显示数 / 总数 · 筛选：xxx”。
- **队列排序**：队列行右键新增上移/下移/置顶/置底；`Queue::move_item`
  按 items 顺序移动并重映射 current/history/shuffle_order/pending_cycle。
  有筛选时排序项置灰（避免与隐藏行交错），单元测试覆盖顺序与 current。
- 侧栏“我的歌单”已按用户要求移除（2026-11-10）；`Session.playlists` 仍保留
  数据供后续文件关联/最近使用，但不显示在 UI。

**验证**

- `cargo build` 0 warning；`cargo test` 174 passed / 0 failed / 6 ignored；
- `--self-test --page now_playing|queue|library|settings` 全部 `painted=true`；
- `--bench 5`：`ticks=192, frames=192, avg_paint=588us, private=24.7MB ws=44.3MB`；
- 实机（portable，175% DPI）：
  - 第二实例转发：第二个进程立即以 0 退出，进程/主窗口仍只有 1 个；
  - `close_behavior=exit` 发送 WM_CLOSE 后进程退出；`tray` + 最小化后窗口
    `IsWindowVisible=false` 且进程存活；
  - 设置页三个分类截图正常，ReplayGain/均衡器/预设修改后 settings.json
    立即更新；
  - 队列 3 首时筛选 `b` 显示 1/3 行。


### 11.4 随机播放缓存与 mp3list 进度记忆

- `Session` 新增 `shuffle_cursor`（当前曲目在随机顺序中的位置）与
  `playlist_shuffle`（每个 `.mp3list` 的随机顺序缓存，最多 8 个）。
- `AppCore::shuffle_snapshot()` 同时保存随机顺序 keys、cursor 和 cursor_key；
  `build_session` 每次保存当前队列的 `shuffle_keys/cursor`，并更新当前歌单的
  缓存。
- `import_playlist` 打开时：
  1. 先把离开的歌单随机状态写入缓存；
  2. 优先使用该歌单缓存的随机顺序；没有缓存时才用当前队列顺序（同一歌单）
     或生成新的随机顺序；
  3. `#SEARCH#` 重新扫描；已存在的曲目保持原有相对顺序，新添加文件追加到
     随机顺序末尾；
  4. 恢复当前曲目：优先 `current_key`，其次该歌单缓存的 `cursor_key`，
     再其次 `cursor` 位置；都失败才从随机顺序第一首开始。
- 进程重启：`AppCore::new` 先按 `shuffle_keys` 恢复顺序与 `current_key`，
  `current_key` 找不到时使用 `shuffle_cursor` 兜底；随后播放列表自动刷新
  仍走同一 import 路径。
- 单测 `reopening_playlist_keeps_shuffle_progress_and_appends_new_files`：
  两首曲目随机后播放第二首，向目录新增第三首并重新打开，断言原顺序不变、
  新歌在末尾、当前曲目不变。
- 单测已覆盖核心路径；UI 重新打开歌单（队列页“导入歌单”/拖入 `.mp3list`）
  即可走同一逻辑。

### 11.5 大封面解码加速（WIC）

- 现象：大尺寸内嵌封面（4000×4000+ JPEG）切歌时封面出现明显延迟。
- 根因：`image` crate 先解码完整图像再 `thumbnail`，4000×3000 在 debug 下约
  3.3s；期间只为得到 640px 封面。
- 修复：`decode_cover_images` 先走 Windows WIC：
  `IWICImagingFactory` → `IWICBitmapClipper`（居中方形裁剪）→
  `IWICBitmapScaler`（FANT）→ `IWICFormatConverter`（32bppRGBA）→
  `CopyPixels`。WIC 不支持的格式/编解码器回退原 `image` 路径。
- 实测（debug，4000×3000 JPEG → 640 方形）：WIC 约 **34ms**，原 Rust 路径约
  **3.35s**；输出 640×640，UI 侧无需再缩放。行缩略图（64px）同样走 WIC。
- `image_util::large_jpeg_is_scaled_and_center_cropped` 单测断言方形输出；
  `large_cover_decode_benchmark`（ignored）保留对比数据。
- 实机：3000×3000 四色测试封面在卡片/播放条/队列行均正确显示；切歌后
  数百毫秒内出现，不再出现明显空白等待。

### 11.6 UI 细节修正（2026-11-10）

- **队列筛选框与副标题重叠**：筛选框从 header 内移到 header 下方的独立工具栏
  （`layout.queue_filter`），队列列表起点改为 `layout.queue_list`；副标题恢复
  独占 header 第二行。曲库布局不变。
- **单行 EDIT 垂直不居中**：Win32 单行 EDIT 文本总贴顶。`place_edit` 改为按
  字体行高（13.5 DIP + 6px 余量）设置 EDIT 高度，并在绘制框内垂直居中；
  搜索框、队列筛选、扩展名输入框统一受益。
- **歌词编辑器没有换行**：LRC 用 `
`，而 Win32 多行 EDIT 只识别 CRLF。
  `set_edit_text` 写入前 LF→CRLF，`edit_text` 读取后 CRLF→LF；磁盘/解析仍保持
  LF，dirty 比较与保存不受影响。实机三行 LRC 正确分行显示。

---

## 12. 后续：自定义与主题（2026-11-10 记录）

> 用户要求把下列项目作为后续批次。原则：所有表面共享同一份设置与渲染参数；
> 改动必须能运行时生效、持久化，并且不破坏局部重绘（主题切换允许一次整窗重绘）。

### 12.1 歌词显示自定义（主窗口 / 独立歌词窗口 / mini player / 桌面歌词）

- **高度/行距**：主行高、翻译行高、行间距、歌词区占页面高度（主窗口卡片下方），
  独立窗口/桌面/mmini 的歌词行数与整体高度；`auto_height` 关闭时使用手动值。
- **字体**：字体族、字号、粗细、是否斜体；主行/翻译行/页码/歌手信息可分别设置。
- **颜色**：当前行、已唱/KTV 填充、下一行、普通行、翻译行、背景文字描边色。
- **阴影/描边**：分别控制主行/翻译行/页脚（标题歌手）的 shadow、outline，
  可调颜色、宽度、偏移；关闭时不产生额外绘制。
- **背景**：透明 / 纯色 / 渐变 / 图片 / 模糊封面；颜色、图片路径、透明度、
  圆角、内边距、窗口阴影、边框。桌面/迷你窗口已有 `OverlaySettings` 字段，
  主窗口/独立窗口需要共用同一套参数（独立窗口当前是硬编码）。
- **KTV 开关**：`karaoke` 总开关；`karaoke_words_only` 只对**增强 LRC**
  （带 word timings）做逐字填充。开启 words-only 而当前歌词只有行时间时：
  - 不做逐字/渐变填充；
  - 按普通计时歌词显示当前行（整行高亮或单行显示），不报错、不退回 KTV。
- **单行模式**：mini/桌面可设 `lyrics_lines = 1`，只显示当前行（含翻译可选）；
  主窗口大歌词/独立窗口可配置显示行数与是否显示翻译。
- **设置入口**：设置页新增“歌词”分类；主窗口歌词面板头部可提供“歌词设置”快捷入口。
- **实现落点**：
  - `Settings.lyrics` / `LargeLyricsSettings` / `DesktopLyricsSettings` /
    `MiniPlayerSettings` 已有大部分字段；新增缺失字段并统一命名。
  - `core::lyrics::view::ViewConfig` 扩展字体/颜色/行高/翻译/KTV 参数；
    `lyrics_view.rs`、`lyrics_screen.rs`、`mini_overlay`、`desktop_overlay`
    只消费同一份配置，避免四套硬编码。
  - 颜色以 RGBA 存储，GDI/D2D 各自转换；字体/画刷按配置版本缓存。
  - 任一项变更：更新 settings → `flush_settings()` → 对应表面
    `mark_dirty()`/整块重绘一次；播放中的 KTV 仍只失效当前行。
- **验收**：175% DPI 下四个表面同一份歌词配置显示一致；修改字号/颜色/背景
  无需重启；暂停时无周期重绘；`words_only` 对普通 LRC 不产生逐字填充。

### 12.2 迷你播放器自定义

- 窗口：宽度、高度、`auto_height`、位置记忆、透明度、置顶、任务栏显示、
  拖动/缩放、滚轮音量、双击标题回到主窗口。
- 内容：封面尺寸/是否显示、标题/歌手、进度条、时间、音量/静音、上一首/播放/
  下一首/关闭按钮的显示开关。
- 歌词：显示/隐藏、1–2 行、是否翻译、是否加粗、字号/字体、KTV 总开关、
  `karaoke_words_only`（仅增强 LRC 逐字）、当前行/普通行/翻译行颜色。
- 颜色：面板/卡片/hover/分割线/文字/弱化文字/强调色，跟随主题或独立覆盖。
- 交互：可选鼠标穿透/锁定；关闭按钮行为（隐藏/恢复主窗口）。
- **实现落点**：`MiniPlayerSettings` + `MiniState`/`MiniColors` 扩展；
  `mini_overlay/render.rs` 按配置布局；设置页“迷你播放器”分类；
  变更后重新发送 State 并 `invalidate_full`。

### 12.3 播放器主题与配色

- **明/暗方案**：`ThemeMode`（Dark/Light/System）；浅色方案需要独立调色板，
  不是简单反色（面板/卡片/边框/悬停/选中/禁用/滚动条分别定义）。
- **强调色**：`ThemeSettings.accent`（已有）替换 `paint.rs` 中全部硬编码
  `C_ACCENT/C_ACCENT_TEXT/C_PLAY_*`；派生悬停/按下/选中色（保证对比度）。
- **配色方案**：面板、卡片、列表行、侧栏、播放条、输入框、禁用态、错误色、
  成功色；支持“跟随强调色生成”或逐项覆盖。
- **外观细节**：圆角、列表密度（紧凑/舒适）、界面缩放 0.75–1.60、
  字体族/字号、背景（纯色/静态模糊/流动渐变，后者可选 D2D/DWM）、
  动画开关/减少动态、帧率上限（15–120）。
- **实现落点**：
  - `paint.rs` 的 `const C_*` 改为 `Palette` 资源，按主题版本缓存画刷；
  - `Painter` 增加 `set_palette`/`palette_rev`；主题变化整窗重绘一次；
  - 播放条/列表/设置/歌词子窗体/独立窗口/overlay 全部使用同一调色板；
  - `ThemeSettings.backend/gpu_power` 对原生 UI 无效，可标记为“仅归档版”。
- **验收**：暗/亮切换后所有页面无残留旧色；强调色变化不需要重启；
  滚动/悬停仍只失效控件矩形；主题切换只在一次整窗重绘内完成。

### 12.4 更多可自定义设置（建议顺序）

1. 设置页增加“歌词 / 迷你播放器 / 外观”分类，复用现有通用行布局与
   `flush_settings`；每个分类支持“恢复默认”。
2. 设置导入/导出（JSON）与“仅导出非默认项”；不要导出会话/缓存。
3. 快捷键自定义（Space/方向键/Ctrl+…），冲突检测；托盘菜单同步显示。
4. 播放：交叉淡化曲线、EQ 10 段推子/预设编辑、ReplayGain 扫描策略、
   A-B 循环、睡眠定时自定义分钟数、音频设备选择。
5. 窗口：关闭行为/启动最小化/记住位置已完成；补充
   多显示器选择、独立歌词窗口/桌面歌词的显示器与全屏策略。
6. 曲库/队列：列表行高、显示的元数据列、tooltip 内容、默认排序/每页条数。
7. 通知/跳转列表/文件关联的细项（通知仅在后台、最近文件数量、关联扩展名）。
8. 缓存管理：封面/歌词/流的占用与清理入口（不删除下载文件）。

**共同约束**

- 新增可选功能必须有设置开关，运行时关闭立即生效；文案走 `i18n::tr`。
- 颜色/字体/布局参数进入 `Settings`，UI 只读快照；修改走 `Command` +
  `flush_settings()`，不能直接改全局静态。
- 主题/字体/DPI 变化允许一次整窗重绘；普通参数变化只失效受影响控件/表面。
- 设置页与四个歌词表面共用同一份参数，禁止再出现“独立窗口硬编码”的分叉。

---

## 13. 主题 / 歌词 / 迷你播放器 / 均衡器（2026-11-10 第四轮四批）

> 目标：把 §12 的“自定义与主题”从规划落到可运行时修改的第一阶段；
> 原则仍是局部重绘、UI 只读快照、设置立即落盘。

### 13.1 运行时主题调色板（`src/theme.rs`）

- 新增 `theme::Palette`：暗/亮两套调色板，全部颜色由 `ThemeSettings.accent`
  生成（`mix` 把强调色按比例混入面板底色，派生导航/选中/按钮/播放按钮色）。
  `paint.rs` 的 31 个 `C_*` 常量全部删除，改用 `self.pal.*`；主题切换只换
  画刷，不影响局部重绘逻辑。
- `App::palette()` 是唯一入口；`Painter::draw_gdi` 每帧刷新调色板，
  `Painter::set_palette` 供 `WM_DRAWITEM` 等不走绘制的路径使用。
- `ThemeMode::System` 读取
  `HKCU\...\Themes\Personalize\AppsUseLightTheme`，2 秒缓存；暗/亮/跟随
  均运行时生效，无需重启。
- 默认强调色改为竹绿 `[127,211,168]`；`Settings::migrate` 把旧版本
  “存了但从未渲染”的蓝色默认值升级为竹绿，自定义颜色保持不变。
- 原生 `EDIT`/排序 `LISTBOX` 的画笔随主题重建（`sync_native_brushes`），
  否则浅色主题下输入框仍是深色底。
- 播放设置弹窗、内嵌歌词子窗体、独立歌词窗口的强调色/文字色也读取同一份
  调色板与 `large_lyrics` 颜色；独立窗口背景仍是沉浸式深色（有意保留）。
- 歌词面板在浅色主题下：若用户没有改过默认歌词颜色，则自动换成深色文字，
  自定义颜色原样使用。

### 13.2 设置页分类与通用行

- 分类从 4 个扩到 9 个：**曲库 / 播放 / 歌词 / 独立歌词 / 桌面歌词 / 迷你 /
  窗口 / 外观 / 系统**，三行布局（82 DIP/个），最小 640 DIP 宽仍可完整显示。
- 通用设置行支持滚动：`App.settings_scroll` + `layout::settings().generic`
  视口；滚轮只在设置内容区生效，切换分类归零，窗口缩放后夹取最大值。
- 右侧新增**可见滚动条**（`SettingsLayout.scrollbar`）：轨道/滑块、悬停/拖动、
  `SetCapture`，与滚轮共用 `settings_scroll`；只有内容超出视口时显示。
- 行矩形改用 `generic` 视口（右侧预留 12 DIP），控件列右对齐、最大 320 DIP，
  因此滚动条不会压住步进器的“+”按钮。
- 行数较多的分类（播放 19 行、桌面歌词 34 行、迷你 12 行）可滚轮或拖动
  滚动条查看。

### 13.3 歌词设置

- 设置 → 歌词：逐字高亮、仅逐字歌词逐字填充
  （`karaoke_words_only`）、显示翻译、字号、行距、对齐、独立窗口字号。
- `lyrics_view.rs` 新增 `Style`（调色板 + 字号/行高/颜色/对齐/KTV/翻译/
  reduce-motion），每次 `sync_content` 整体比较；变化时清字体缓存并整块
  重绘一次，播放中的 KTV 仍只失效当前行。
- 字号映射保留旧观感：内嵌当前行 = `font_size - 2`，普通行 = 当前行 - 1；
  独立窗口 = `large.font_size - 3`。行高按字号推导，行距直接使用设置值。
- `words_only`：只有带词级时间的增强 LRC 才做逐字填充；普通计时 LRC 仍显示
  当前行/整行高亮，不回退、不报错。
- 对齐支持左/中/右；右对齐关闭横向跟随，左/中仍按已唱边缘 7/8 触发。
- “减少动态效果”关闭当前行横向跟随（进度/KTV 颜色仍更新）。
- 独立窗口字号、颜色、行距、翻译与 KTV 设置与内嵌面板共用
  `LargeLyricsSettings`，不再硬编码。

### 13.4 迷你播放器设置

- 设置 → 迷你：显示歌词、逐字高亮、仅逐字填充、歌词行数（1/2）、加粗、
  显示翻译、歌词字号、自动高度、置顶、不透明度、窗口宽度、任务栏显示。
- 命令写入 `MiniPlayerSettings`；`overlay_host` 原本已把这些字段映射进
  `MiniState`，本轮补上“暂停时设置也要立即生效”：主窗口记录
  `MiniSettingsKey`，变化时 `force_state()` 并推送，不依赖 250/500ms 心跳。
- 最小化/隐藏主窗口时迷你播放器仍按自身线程继续刷新。

### 13.5 均衡器与 ReplayGain 扫描

- 设置 → 播放新增 10 段均衡器推子（32 Hz … 16 kHz，±24 dB），
  每次调整立即 `engine.set_dsp`；手动调整会把预设切到“自定义”。
- 预设改为循环步进（平坦/摇滚/流行/爵士/古典/人声/低音/自定义）；
  选择“自定义”不会把推子重置为零。
- 新增“扫描无标签曲目”（`playback.replaygain_scan`）开关。
- 单测：`eq_band_edits_become_custom_and_presets_reset_bands` 覆盖夹取、
  预设覆盖推子、Custom 保留推子；`migrate_upgrades_the_old_unused_accent_but_keeps_custom_colors`
  覆盖强调色迁移。

### 13.6 桌面歌词设置

- 新增设置分类“桌面歌词”（27 行）：显示、锁定、鼠标穿透、置顶、行数（1–3）、
  翻译、标题/歌手、字号、加粗、对齐、背景（透明/纯色/渐变/图片/模糊封面）、
  不透明度、圆角、内边距、行间距、文字阴影、描边、面板边框、窗口阴影、
  自动尺寸、宽度、高度、暂停时隐藏、录屏捕获保护、KTV、仅逐字填充、重置位置。
- `DesktopLyricsSettings` 原本已由 `overlay_host::desktop_settings` 全量映射到
  `OverlaySettings`；本轮只补命令（`DesktopLyricsSet*`）与设置页，不复制渲染逻辑。
- 字号步进会按比例同步 next/prev 字号；宽度/高度步进复用
  `DesktopLyricsSetSize`（自动关闭 `auto_size`）。
- 开关“显示桌面歌词”走 `ToggleDesktopLyrics`；主窗口在 `WM_LBUTTONUP` 之后
  立即 `sync_overlays`，暂停时也能启动/销毁 overlay，不用等动画定时器。
- 注意：`--bench` / `--self-test` 的 `state.persist` 为 false，`sync_overlays`
  直接返回，**不会创建**桌面歌词/迷你窗口。验证 overlay 启动/销毁必须用
  普通运行（或便携目录），不能只看 bench。

### 13.7 新增测试参数

```text
--settings-tab <0..8>   启动时打开设置第 N 个分类
                        （曲库/播放/歌词/独立/桌面/迷你/窗口/外观/系统）
--theme <dark|light|system>  覆盖本次运行的主题（不写回配置）
```

`--bench --page ...` 现在在 demo 夹具之后应用，可以配合 `--settings-tab`
直接截取任意设置分类。

### 13.8 验证

- `cargo build` 0 warning；`cargo test` **179 passed / 0 failed / 6 ignored**。
- `--self-test --page settings --settings-tab 0..8` 全部 `painted=true`；
  `--self-test --page now_playing --theme light` 通过。
- `--bench 4`：`ticks=162, frames=161, avg_paint=498us, private=25.4MB ws=45.2MB`。
- 175% DPI 实机截图：暗/亮主题、两行分类、歌词/桌面/迷你/播放设置行、
  通用行滚轮滚动（32 Hz–2 kHz 段）、浅色主题下当前歌词行深色文字均正常；
  640×460 DIP 最小窗口下桌面歌词分类的控件不再越界。
- 普通便携运行（非 bench）点击“显示桌面歌词”：`EnumWindows` 出现
  `BambooNativeDesktopLyrics` 窗口；再点一次切换为“关”并销毁窗口，
  证明设置页→Command→overlay_host→原生窗口链路可用。

### 13.9 仍未做

- 字体的逐面（主行/翻译/页脚）选择、描边/阴影宽度/颜色编辑、
  背景图片选择与模糊强度。
- ~~迷你播放器的颜色独立覆盖~~ **已完成（§18.3）**。
- ~~设置页“恢复默认”、设置导入导出~~ **已完成（§18.1）**；快捷键自定义未做。
- ~~外观的界面缩放 0.75–1.60、动画帧率上限~~ **已完成（§18.2）**；
  背景模糊/封面背景尚未接原生绘制。

---

## 14. 歌词颜色 / 独立歌词 / 设置滚动条（2026-11-10 第五轮）

### 14.1 原生取色器

- 新增 `Win32_UI_Controls_Dialogs` 依赖，使用系统 `ChooseColorW`
  （`CC_RGBINIT | CC_FULLOPEN | CC_ANYCOLOR`），16 个自定义色保存在
  `State.custom_colors`，跨多次取色保留。
- 设置页的颜色行是 `SettingKind::Color`：按钮本身是色块 + `#RRGGBB`，
  点击只设置 `App.color_request`，**不在借用 `State` 期间弹模态框**；
  `WM_LBUTTONUP` 在借用结束后取走请求、打开对话框，再 `apply_color`。
- 取消对话框不改任何设置；确认后走 `Command` → `flush_settings()` →
  `invalidate_all`，并再次 `sync_lyrics` / `sync_overlays` 立即生效。
- 桌面歌词颜色是 RGBA：取色器只改 RGB，**保留原 alpha**（描边/阴影默认
  230/150），不要用不透明色覆盖。

### 14.2 独立歌词设置

- 新增设置分类“独立歌词”（`LargeLyricsSettings`，13 行）：字号、行距、
  对齐、翻译、KTV、仅逐字填充、显示封面/标题/播放控制、高亮/当前行/
  普通行/翻译行颜色。
- `lyrics_view` 的 floating 分支继续消费 `LargeLyricsSettings`；独立窗口壳
  `lyrics_screen` 新增 `ScreenStyle.show_cover/title/footer`，按设置跳过
  封面、标题/歌手/专辑或底部进度/播放控制绘制，命中测试同步禁用
  （隐藏的播放按钮不会被点击）。
- 内嵌歌词的 `Style` 增加 `highlight_color`；KTV 填充不再固定使用主题
  accent，而是读 `LyricsSettings.highlight_color`。

### 14.3 歌词 / 桌面歌词颜色

- `LyricsSettings`、`LargeLyricsSettings` 新增 `highlight_color`
  （默认竹绿 `[143,224,181]`）；嵌入式歌词与独立窗口的 KTV 填充都使用它。
- 设置 → 歌词新增：高亮颜色、当前行、普通行、翻译行；设置 → 独立歌词
  新增同样四项；设置 → 桌面歌词新增：当前行、下一行、上一行、标题/歌手、
  翻译行、描边、阴影。
- 桌面 overlay 的“当前行颜色”同时是它的 KTV 填充色（渲染层原本就用
  `current_color`），所以自定义当前行颜色会同时改变已唱高亮。
- 桌面歌词补齐其余外观项：背景图片（rfd 选择，自动切到“图片”模式）、
  清除图片、背景颜色/渐变第二色（取色器）、字体（`ChooseFontW`）、
  描边宽度、阴影偏移、翻译行/标题歌手的描边与阴影、全屏时隐藏。
- 暂停时改颜色/背景/字体也要立即回显：主窗口保存 `DesktopSettingsKey`
  （含锁定/字体族/行数/背景模式与图片/颜色/尺寸/描边宽度等），变化时
  `force_state()`；与迷你播放器的 `MiniSettingsKey` 同一套机制。

### 14.4 设置滚动条

- 设置页右侧新增自绘滚动条（10 DIP，右对齐），内容不超过视口时隐藏；
  滑块最小 24 DIP，比例与 `settings_scroll / settings_max_scroll()` 一致。
- 点击轨道/滑块即开始拖动并 `SetCapture`，`WM_MOUSEMOVE` 映射到滚动位置；
  `WM_LBUTTONUP` 释放捕获。悬停/拖动颜色与主列表滚动条一致。
- **拖动必须在点击处理的最前面启动**：滚动条位于 `generic` 之外，
  若先执行“不在 generic 就 return”的守卫，拖动永远不会开始。
- 滚轮滚动后要**同时失效 `generic` 与 `scrollbar`**；只失效行区域时
  滑块要等鼠标移动（hover 失效）才更新。
- **行矩形必须使用 `SettingsLayout.generic`（右侧预留 12 DIP）**：
  早期用 `folders.right` 导致控件“+”按钮被滚动条压住。
- 滚轮、拖动滚动条、切换分类归零三者共用同一状态；窗口缩放/DPI 变化后
  clamp 到新的最大值。

### 14.5 数值输入框与范围

- 所有数值型 step 行（字号、行距、透明度、宽高、圆角、内边距、描边宽度、
  阴影偏移、交叉淡化、预增益、10 段 EQ 增益、迷你宽度等）的数值区域
  可直接点击输入：第三个原生 `EDIT`（ID 103）覆盖在 value 矩形上，
  `ES_CENTER`、无边框、透明背景；点击即全选，Enter 提交、Esc 取消、
  失焦提交。百分比行（透明度）按显示单位（0–100）输入。
- 输入只在点击处理返回后显示，避免在 `&mut State` 借用期间创建/聚焦子窗体；
  `sync_number_edit` 跟随行位置，滚出视口或切页时自动收起。
- 提交时解析 `f32`，非法/NaN 直接忽略；数值仍走原 `Command`，
  因此范围夹具只保留一处。
- 范围放宽：主歌词 8–120、独立歌词 10–200、桌面歌词 8–200、迷你 8–64；
  主/独立行距 120/160、桌面行距 −96–160、内边距 0–400、圆角 0–200、
  桌面尺寸 160–6000 × 24–2000、迷你宽度 200–1600、EQ/预增益 ±24 dB。
  歌词子窗体/独立窗口/迷你渲染中的二次 clamp 同步放宽，
  避免输入了但显示不变。
- `SetLyricsFontSize` / `SetLyricsLineSpacing` 不再联动独立窗口设置
  （现在“独立歌词”分类单独设置）。

### 14.6 验证

- `cargo build` 0 warning；`cargo test` **183 passed / 0 failed / 6 ignored**。
- `--self-test --page settings --settings-tab 0..8` 全部 `painted=true`。
- 175% DPI 实机：
  - 9 个分类三行布局、颜色色块（#FFFFFF/#96A0B4/#828EA5 等）正常；
  - 设置滚动条显示/滚轮/拖动一致（滚轮后滑块立即移动；拖动可拉到
    列表底部），最小窗口下“+”按钮与滚动条有 12 DIP 间距；
  - 点击颜色块弹出系统取色器（对话框类 `#32770`），取消后不修改设置。
- 字号输入框：点击“歌词字号”数值区出现居中 EDIT（显示 18），输入 40 后
  Enter，EDIT 收起且行值变为 40；Esc/失焦路径同样收起。
- 单测 `lyrics_color_commands_store_rgb_and_keep_desktop_alpha`、
  `desktop_background_extras_fill_the_gaps` 覆盖颜色/背景/字体/夹取。
- 桌面歌词背景图片、清除图片、背景色/渐变第二色、字体选择、描边宽度、
  阴影偏移、翻译/页脚描边阴影、全屏隐藏均已在设置页出现；
  `--self-test --settings-tab 4` 绘制通过。

### 14.7 仍未做

- 逐面字体族/粗细/斜体（桌面歌词已可选字体族，主行/翻译/页脚尚未分开）。
- 背景模糊强度、歌词面板边框的逐项颜色（圆角不再规划，见 §8.9）。
- ~~迷你播放器独立配色~~ **已完成（§18.3）**。
- ~~设置导入/导出、“恢复默认”~~ **已完成（§18.1）**；快捷键自定义未做。

---

## 15. 文件夹浏览页（2026-11-10 第六轮）

### 15.1 核心

- `core::library::browser` 增加 `BrowseService::with_wake`：后台线程读完目录后
  调用 `wake()`（`WM_APP_WAKE`），暂停/无动画定时器时也能更新。
- `AppCore` 新增 `browser: FolderBrowser` / `browse_service` / `browse_rev`，
  `tick` 轮询结果；路径不匹配的迟到结果直接丢弃。
- 命令：`BrowseTo(Option<PathBuf>)`（None = 磁盘列表）、`BrowseUp`、
  `BrowsePlay(index)`（目录进入 / 音频播放 / 列表导入）、`BrowseEnqueue`、
  `BrowseAddAll`、`BrowsePlayAll`（替换队列并播放第一首）。
- `SetPage(Folders)` 首次进入自动列磁盘；再次进入保留上次目录。
- 磁盘列表只探测 `C:`–`Z:`，跳过 A/B（软驱探测可能阻塞）。

### 15.2 UI

- 侧栏新增“文件夹”，`NAV_ITEMS` 5→6；`Page::Folders` 映射到核心页面。
- 布局：标题下方工具栏（上一级 / 刷新 / 播放全部 / 加入队列）+ 虚拟列表 +
  右侧滚动条；每行 40 DIP（跟随列表密度），只绘制可见行。
- 行：30×30 图标（Segoe MDL2：文件夹/音符/列表）+ 名称 + 右侧类型
  （文件夹/音频/播放列表）；悬停/按下与队列行一致，tooltip 显示完整路径。
- 单击目录进入、单击音频播放、单击 `.mp3list/.m3u/.m3u8` 导入；Backspace
  返回上一级。滚动条复用队列的拖动/滚轮逻辑（`App::scrollbar_rect`）。
- 目录读取完成由 `browse_rev` 变化触发：重置 hover/scroll，失效 header、
  列表与滚动条；不整窗重绘。
- 启动参数新增 `--page folders` 与 `--folder <dir>`（直接打开指定目录）。
- 修复启动参数解析：`--page folders`、`--folder <dir>`、`--library <dir>` 的
  **值**不再被当作“要入队的文件/目录”（此前 `--folder` 会顺带把整个目录塞进
  队列并自动播放）。

### 15.3 验证

- `cargo build` 0 warning；`cargo test` **182 passed / 0 failed / 6 ignored**。
- `--self-test --page folders`（暗/亮）均 `painted=true`。
- 175% DPI 实机：进入 `F:\codes\rust\mp3player` 列出 8 个目录；单击 `.git`
  进入、点“上一级”返回；含 2 个 WAV + 1 个 mp3list 的测试目录正确显示
  音频/播放列表图标，“播放全部”替换队列并开始播放。
- `--bench 4`：`avg_paint=464us, private=25.3MB, ws=45.3MB`。
- 单测 `folder_browser_lists_entries_and_enqueues_audio` 覆盖异步目录读取、
  文件夹/音频排序与“加入队列”。

### 15.4 仍未做

- ~~文件夹页右键菜单（加入曲库/打开资源管理器/复制路径）~~ **已完成（§16.1）**。
- ~~目录历史的“前进”与地址栏输入跳转~~ **已完成（§16.2）**。
- ~~列表行缩略图（当前用图标，避免为每个目录项解码封面）~~ **已完成（§18.4）**。

---

## 16. 文件夹页补齐（2026-11-10）

### 16.1 行右键菜单

- `WM_CONTEXTMENU` 的 `row_menu_data` 覆盖 `Page::Folders`，`App::list_row_at`
  允许文件夹页；右键行时优先弹文件夹菜单（不再落到曲目信息菜单）。
- 目录行：`打开文件夹` / `加入曲库` / `复制路径` / `在资源管理器中打开`。
- 音频行：`播放` / `添加到播放队列` / `复制路径` / `打开文件位置`。
- 播放列表行：`导入播放列表` / `复制路径` / `打开文件位置`。
  不提供“加入队列”：导入 `.mp3list/.m3u` 按设计会替换队列。
- 新增结果类型 `FolderPlay/FolderEnqueue/FolderAddLibrary`，都在 `State` 借用
  结束后执行；`加入曲库` 走 `Command::LibraryAddFolder` 后立即
  `flush_settings()`；目录用 `open_path` 直接 `ShellExecute` 打开，
  文件仍用 Explorer `/select` 定位。
- 菜单文案新增到 `src/core/i18n/en.json`：`加入曲库`、`在资源管理器中打开`、
  `导入播放列表`、`后退`、`前进`、`输入路径，按 Enter 跳转`。

### 16.2 后退/前进与地址栏

- `FolderBrowser` 增加两个历史栈：`back: Vec<Option<PathBuf>>`、
  `forward: Vec<Option<PathBuf>>`（`None` = 磁盘列表）。
  - `BrowseTo` 只在位置变化时压栈并清空 forward；刷新同一路径不算新历史。
  - 新命令 `BrowseBack` / `BrowseForward` 弹出栈顶并互相压栈。
  - 单测 `folder_browser_history_goes_back_and_forward` 覆盖 back/forward、
    新导航清空 forward、同路径刷新不增加历史。
- 文件夹工具栏改为两行：
  - 第一行：`后退` / `前进` / `上一级` / `刷新`；无历史时两个历史按钮置灰
    且不进入命中测试。
  - 第二行：`播放全部` / `加入队列` + 地址栏；地址栏吸收剩余宽度
    （640 DIP 最小窗口下仍有约 130 DIP）。
  - `layout::folder_toolbar()` 返回两行工具栏的包围矩形，供绘制脏区判断使用。
- 地址栏是常驻原生 `EDIT`（ID 104），无边框/透明背景 + 自绘边框：
  - `folder_path_proc` 子类化 Enter/Esc，`PostMessage(WM_APP_FOLDER_GO/CANCEL)`
    给父窗口；在借用 `State` 期间不直接导航。
  - Enter：空文本 → 磁盘列表；文件路径 → 其父目录；目录 → 直接进入。
  - Esc：不修改浏览位置，`sync_edit_boxes` 把文本还原为当前路径。
  - 未聚焦时 `sync_edit_boxes` 同步当前路径（点击行/后退/前进/上一级后立即更新）；
    聚焦时保留用户输入，不覆写。
  - 聚焦时边框使用强调色（`Input::FolderPath`）。
- 新增 `App::folder_go` / `folder_can_go_back` / `folder_can_go_forward` /
  `folder_address_text` / `reset_folder_view`。

### 16.3 工具栏按钮悬停后消失（局部重绘 bug）

- 现象（用户报告）：鼠标移到文件夹页工具栏按钮上，按钮和文字一起消失；
  移开后也不恢复，只有整窗重绘（切页/缩放）才回来。
- 根因：按钮位于 `layout.header`（y 0–64）之外（工具栏从 72 开始），但绘制
  代码放在 `draw_header` 内，而 `draw_header` 用
  `if !hits(layout.header, dirty) { return; }` 提前返回。悬停只失效按钮矩形，
  背景被重绘、按钮被跳过。
- 修复：`layout::folder_toolbar()` 返回两行工具栏包围盒，`draw_header` 的
  脏区判断改为 `header` 与 `folder_toolbar` 任一相交就绘制；每个按钮仍各自
  用 `hits(rect, dirty)` 控制。
- 约定：**任何在 `draw_header` 中绘制、但位于 `layout.header` 之外的控件，
  都必须把自身区域纳入脏区判断**；否则 hover/pressed/焦点失效会先擦掉它。
  同样适用于以后新增的工具栏/地址栏类控件。

### 16.4 验证

- `cargo build` 0 warning；`cargo test` **184 passed / 0 failed / 6 ignored**。
- `--self-test --page now_playing|queue|library|folders|settings` 全部退出码 0；
  folders 页暗/亮主题均 `painted=true`。
- `--bench 5`：`ticks=201, frames=208, avg_paint=571us, private=25.1MB ws=45.0MB`。
- 175% DPI portable 实测（真实鼠标 + 窗口消息）：
  - 点击 `sub` 行进入目录，地址栏从 `...\music` 变为 `...\music\sub`；
  - 后退回到 `music`、前进回到 `sub`，两个按钮的启用/置灰状态同步变化；
  - 地址栏输入路径并按 Enter 跳转 `sub`；Esc 保持原路径；
  - 右键目录行弹出“打开文件夹 / 加入曲库 / 复制路径 / 在资源管理器中打开”；
  - 悬停 `后退` 按钮时保持可见并显示 hover 底色（16.3 已修复）。
- 仍未做：多选、拖放排序、行缩略图（避免为每个目录项解码封面）。

---

## 17. 曲库工具栏：整库入队 / 缺失检查 / 移除索引（2026-11-10）

### 17.1 整库“全部入队”

- 原实现只复制 `library.rows`（当前页）。曲库改为整页查询后，这等于只
  入队当前页。新增 DB 请求 `QueryAll { request_id, search, sort }`：
  - 与分页查询共用 `search_clause()` 的 WHERE 与 `LibrarySort::order_by()`，
    但不带 `LIMIT/OFFSET`；结果通过 `DbEvent::AllDone` 回到 UI 线程。
  - `Command::LibraryQueueAll` 只投递查询（`library.all_id` 去重），
    `AppCore::tick` 收到 `AllDone` 后 `queue.add_many(...)`。
  - 语义：加入“当前搜索 + 当前排序”下的全部曲目；不过滤分页窗口，
    也不清空现有队列（去重由 `Queue::add_many` 负责）。
- 单测 `library_queue_all_uses_the_whole_result_not_the_page`：加载 2 行
  页面后调用命令，断言队列得到 A–E 五首。

### 17.2 检查缺失文件

- `Command::LibraryCheckMissing` → `LibraryDb::check_missing()`；DB 在
  `recheck_missing` 后回发 `MissingDone(entries)` 与 `Changed`。
- `AppCore` 记录 `library.missing_checked = Some(n)`（`None` = 尚未检查），
  并 bump `library_rev`；`Changed` 触发当前页重查（缺失行被 `missing = 0`
  过滤掉，总数自然减少）。
- 页头副标题追加“ · 缺失文件：N 首”，不需要弹窗/Toast。
- 缺失检查不会删除索引行，文件恢复后再次检查会重新出现在列表；需要移除
  索引时用行右键“从曲库移除”。

### 17.3 从曲库移除

- 曲库行右键新增“从曲库移除”，`MB_YESNO` 二次确认，提示
  “只从索引移除，不会删除文件。”。
- `RowMenuOutcome::LibraryRemove(index)` → `Command::LibraryRemove(index)` →
  `LibraryDb::remove_keys(vec![key])`；DB 回发 `Changed`，当前页重查。
- 正在播放的曲目不受影响（队列里是独立 `Track`），只是不再出现在曲库。

### 17.4 扫描中取消索引

- 曲库扫描进行时（`Snapshot::library_index_active`），页头“重新扫描”按钮
  文案变为“取消索引”，点击走 `Command::LibraryCancelIndex` →
  `LibraryDb::cancel_index()`；扫描线程在文件之间停止，`IndexDone`
  （`cancelled = true`）仍会回写已扫描结果并刷新列表。
- 不新增按钮：复用原按钮位置，避免 640 DIP 最小窗口下页头再次拥挤。

### 17.5 布局

- 曲库页在页头与列表之间新增一行工具栏（`layout.library_toolbar`），
  “全部入队 / 检查缺失”右对齐；列表起点下移 32 DIP，分页条与每页行数
  自动按新高度重算。
- 与文件夹工具栏一样，这一行在 `layout.header` 之外但由 `draw_header` 绘制，
  已纳入 `toolbar_dirty` 脏区判断，hover 不会擦掉按钮（§16.3 的同类问题）。

### 17.6 验证

- `cargo build` 0 warning；`cargo test` **186 passed / 0 failed / 6 ignored**。
- `--self-test --page library` 暗/亮主题均退出码 0；五个页面全部通过。
- `--bench 5`：`ticks=193, frames=193, avg_paint=543us, private=24.2MB ws=44.3MB`。
- 175% DPI portable 实测（5 首 WAV，干净 session）：
  - 曲库页显示“全部入队 / 检查缺失”工具栏；
  - 点击“全部入队”后切到播放队列，5 首全部在队列中；
  - 删除 `C.wav` 后点击“检查缺失”，列表变为 A/B/D/E，副标题显示
    “缺失文件：1 首”；
  - 右键曲目选择“从曲库移除”，确认后该行消失（文件仍在磁盘上）。

---

## 18. 设置数据、界面缩放、迷你配色、文件夹缩略图（2026-11-10）

### 18.1 设置导入 / 导出 / 恢复默认

- 设置 → 系统新增三行：`导出设置…`、`导入设置…`、`恢复默认设置…`。
- 点击只设置 `App.settings_action`，`WM_LBUTTONDOWN` 借用结束后再打开
  rfd 文件对话框/确认框（与其他模态对话框同一规则）。
- 核心（`core::app`）：
  - `export_settings(path)`：把整个 `Settings` 以 pretty JSON 写出（原子写、
    不含 session/队列）。
  - `import_settings(path)`：`config::load_settings_strict` 严格解析（坏文件
    返回错误，**不会**像启动加载那样静默回落默认值），成功后整体替换设置。
  - `reset_settings()`：`Settings::default()`。
  - 三者的公共尾部 `apply_settings()`：立即应用音量/静音/DSP，重建目录监听、
    重新索引，并 `flush_settings()`。
- UI 侧 `apply_reloaded_settings()`：清字体缓存、按新的 ui_scale 重算
  有效 DPI/布局、重建本地位图、重同步歌词/播放设置弹窗/overlay/系统集成/
  托盘，然后整窗重绘。导入失败用 `MessageBoxW(MB_ICONERROR)` 报错。
- 单测 `settings_export_import_and_reset_round_trip`：导出→改值→导入恢复、
  坏 JSON 被拒绝且设置不变、恢复默认回到默认主题/强调色/密度。

### 18.2 外观：界面缩放 + 动画帧率上限（界面缩放已于 §27 移除）

- 设置 → 外观新增两行（均支持 ± 与数值输入）：
  - **界面缩放** 75%–160%，步进 5%（`Command::SetUiScale`）。
  - **动画帧率上限** 15–120 fps，步进 5（`Command::SetFrameRate`）。
- 缩放实现：`有效 DPI = 系统 DPI × ui_scale`，在启动、`WM_SIZE`、
  `WM_DPICHANGED`、`apply_reloaded_settings` 统一计算；布局仍全用 DIP，
  因此字体、列表行、原生 EDIT/LISTBOX、鼠标坐标换算一起缩放。
  - `WM_GETMINMAXINFO` 用 `app.dpi`（已经是有效 DPI），最小窗口同步放大。
  - `grow_to_min_client()`：启动恢复/缩放变化后，如果客户区小于 640×460
    逻辑 DIP，按当前有效 DPI 放大外框，避免布局被 clamp 到窗口外。
  - 原生控件字体、`place_edit`、时间/提示坐标都直接使用 `app.dpi`。
- 帧率：`sync_timer` 使用 `performance.frame_millis()`（`frame_rate` 夹取
  15–120，最小 8ms）；设置变化后主窗口立即 `sync_timer(hwnd)` 重启定时器。
- 单测 `ui_scale_and_frame_rate_clamp` 覆盖 0.75–1.6 / 15–120 夹取。
- 实机：外观页 `界面缩放 100% → 105%` 点击“+”后整窗立即放大、布局重排，
  原生输入框/列表同步。

### 18.3 迷你播放器独立配色

- `MiniPlayerSettings` 新增三个 RGB：`panel_color`、`text_color`、
  `accent_color`（默认值与旧硬编码外观一致：`#25272B`/`#E8EBEE`/`#7FD3A8`）。
- 设置 → 迷你新增三行色块（系统取色器）。颜色进入 `MiniSettingsKey`，
  暂停时改色也会 `force_state()` 立即推送，不等 250/500ms 心跳。
- `overlay_host::mini_colors(&settings)` 由三个颜色派生完整 `MiniColors`：
  - 卡片/悬停/分割线 = 面板混白 5%/12%/7%；
  - 弱化/更弱文字 = 文字向面板混 63%/43%；
  - 强调色直接使用。
  这样任何面板色都有可读的层次，不需要再暴露十几个颜色项。
- 单测 `custom_mini_colors_drive_the_derived_palette`：默认强调色、面板/
  文字/强调色直传、卡片比面板亮、弱文字介于文字与面板之间。
- 实机：`settings.json` 写入 `panel=[20,40,80]`、`text=[255,220,120]`、
  `accent=[255,80,80]` 后启动，迷你播放器面板深蓝、文字黄色、
  进度条/播放键红色。

### 18.4 文件夹页封面缩略图

- `App::row_cover` 对文件夹页的音频行返回 `core.row_cover(path)`；
  目录/播放列表仍用 Segoe MDL2 图标。
- `request_visible_covers` 只为可见的音频行请求缩略图；
  目录读取完成（`browse_rev` 变化）后立即请求，滚动时同样按可见窗口请求。
- `row_cover_rev` 变化时失效 `folder_list`（原先只处理队列/曲库）。
- 无封面/解码失败仍回退音符图标；`load_covers=false` 时完全不请求。
- 实机：目录放 `testsrc2` 彩色 `cover.png`，A/B/D/E.wav 行显示 30×30
  缩略图；目录与 `.mp3list` 仍为图标。

### 18.5 验证

- `cargo build` 0 warning；`cargo test` **189 passed / 0 failed / 6 ignored**。
- `--self-test` 五页全部退出码 0。
- `--bench 5`：`ticks=301, frames=301, avg_paint=429us, private=25.2MB ws=45.2MB`
  （定时器现在严格按 60 fps 运行）。
- 175% DPI portable 截图：
  - 设置 → 系统底部三行设置数据操作可见；
  - 设置 → 外观显示 `界面缩放 100%`、`动画帧率上限 60 fps`，点击 + 后变
    105% 并整窗放大；
  - 迷你播放器自定义面板/文字/强调色生效；
  - 文件夹页音频行显示真实缩略图。

---

## 19. 最后一批（已完成，见 §26）：overlay GPU / 主窗口 CPU / UI 文案

> 用户 2026-11-10 指定：以下三项放在所有功能批次之后做（“最后做”）。
> 原则不变：先测量再优化；遵守 AGENTS 的局部重绘与性能验收，
> **不接受用降帧率、降画质换占用**；空闲时不得有周期重绘。

### 19.1 迷你播放器 / 桌面歌词 GPU 占用

- 现状：两者都是原生 `WS_EX_LAYERED` 窗口，软件 `ID2D1DCRenderTarget`
  画到 DIB，再用 `UpdateLayeredWindowIndirect` 按脏条发布；渲染线程用固定
  16ms 定时器（`TIMER_RENDER`），暂停时也会按状态/位置消息重绘。
- 测量（改之前先做）：
  - 场景：暂停、播放（无歌词）、播放 + KTV、完全遮挡、桌面歌词与迷你同时开。
  - 采集：`Get-Counter '\GPU Engine(*)\Utilization Percentage'` 按 PID 过滤，
    或任务管理器“GPU 引擎”；同时记录进程 CPU 与每秒
    `UpdateLayeredWindow` 次数。
  - 记录基线到本节，优化后复测同一场景。
- 方向（以测量结果为准，不要先降帧）：
  - 暂停 / 无歌词 / 进度不推进时停掉渲染定时器；收到 State/Position 才画一帧。
  - 每帧先判断可见内容是否真的变化（时间标签、进度像素、KTV 填充、当前行），
    无变化直接跳过 `BeginDraw` + 发布；当前是每 16ms 都重画整个小表面。
  - 完全被遮挡或最小化时停止发布；重新可见时整块重绘。注意分层/透明窗口
    不能当作不透明遮挡物（见 §10.4）。
  - 桌面歌词和迷你播放器共用同一份位置心跳，避免两个线程各自高频插值；
    心跳只推进时钟，不要强迫重画静态部分。
  - 保持软件 D2D target；不要为小表面启用硬件 target（固定多占约 30MB，收益为零）。
- 验收：暂停/遮挡时 GPU 0%、无周期发布；播放 + KTV 的 GPU 低于基线，
  动画观感不明显变差；内存不上升。

### 19.2 主窗口 CPU 占用

- 现状：`ANIM_TIMER` 间隔取 `performance.frame_millis()`（默认 16ms）；
  `App::tick` 比较各 rev 后精确失效；`--bench 5` 当前
  `ticks=301, frames=301, avg_paint=429us, private=25.2MB ws=45.2MB`。
- 测量：同一台机器、175% DPI，记录
  - `--bench 5` 的 tick/帧数、avg_paint；
  - 播放 + 滚动歌词时 UI 线程 CPU%（Process CPU 增量）；
  - 暂停、隐藏到托盘、完全遮挡三种空闲状态是否还有周期工作。
- 方向：
  - WM_TIMER 每帧都在跑 `sync_system` / `sync_overlays` / `sync_lyrics` /
    `sync_panel`；把 SMTC 时间轴、跳转列表、通知、系统集成检查按
    250ms / 1s 节流，不要每帧全跑。
  - 检查 WM_TIMER 各分支是否重复 `sync_timer`（with `SetTimer` 每 tick
    重置），只在状态变化时重设定时器。
  - 主窗口隐藏/最小化且没有 overlay/独立歌词时，不需要 60 tick/s 推进不可见
    表面；用 1s 心跳维持 SMTC/时钟，音频与 UI 事件继续走 `WM_APP_WAKE`
    （这是停止不可见工作，不是降低可见动画帧率）。
  - 检查每帧是否触发不必要的 `request_visible_covers`、布局重算、调色板/
    字体缓存重建；缓存按版本失效，不要按帧重建。
  - 保持局部失效规则：不为了省 CPU 改成“少画几块/降低画质”。
- 验收：播放 + 滚动歌词的 CPU 明显低于基线；暂停/隐藏时无周期工作；
  `avg_paint` 不上升，动画和交互观感不变。

### 19.3 UI 文案润色

- 用户明确要求去掉命令式/实现细节描述。已定位的文本（`src/paint.rs`）：
  - 侧栏页脚 `只重绘变化区域` → 显示版本号，例如
    `format!("v{}", env!("CARGO_PKG_VERSION"))`。
  - 侧栏 logo 副标题 `NATIVE UI · M1` → 面向用户的产品描述或版本号，
    不出现内部里程碑。
  - 队列副标题 `{} 首 · 虚拟列表 · 只绘制可见行` → `{} 首歌曲`；
    筛选时保留 `显示 x / y 首 · 筛选：…`。
  - 正在播放 `歌曲卡片 · 可滚动歌词 · 点击行跳转` → 面向用户的说明
    （如“歌词可滚动，点击歌词跳转”）。
  - 搜索页 `P1 页面 · 占位` → 面向用户的状态（如“搜索功能开发中”）。
  - 设置页 `曲库 / 播放 / … · 修改后自动保存` → `修改后自动保存`
    或简短分类摘要。
- 同时检查其他技术用语：`M1`、`NATIVE`、`虚拟列表`、`占位`、`只绘制`、
  `只 blit`、`Rimor`、`WGPU/DX12`，以及托盘/按钮提示里的实现说明。
- 约束：文案仍走 `crate::i18n::tr` / `trf!`，同步 `src/core/i18n/en.json`；
  长文案按现有布局截断，不改变控件宽度假设；不引入新的周期重绘。
- 验收：五个页面、侧栏、播放条不再出现内部实现术语；版本号可见；
  `cargo build` / `cargo test` / `--self-test` 通过。

### 19.4 顺序

1. 先测 19.1 与 19.2 的基线（同一轮、同一环境）；
2. overlay GPU → 主窗口 CPU（共享定时器/心跳改动时一起验证）；
3. 最后做 19.3 文案（避免文案改动干扰性能测量）；
4. 更新本节基线数据与 HANDOFF 顶部验证记录。

---

## 20. P2 系统与外观：Toast / 统计页 / 性能设置 / 背景与可视化（2026-11-10）

> 用户指定：快捷键自定义与无障碍（含键盘/手柄兼容）放到最后一批，与 §19 的
> GPU/文案优化一起做；本轮先落地 Toast、播放统计、性能设置界面、背景与音频
> 可视化。

### 20.1 应用内 Toast / 状态提示

- `App` 增加单条 Toast（`src/app.rs: Toast`）：文本、错误标记、过期时间；
  默认 3.6s 自动过期，新消息替换旧消息，连续重复消息折叠（core 侧）。
- 接入来源（`AppCore::notify` + `notice_rev` + `drain_notices`）：
  - 曲库索引完成/取消、缺失文件检查结果、整库入队数量、曲库 DB 错误；
  - 音频引擎错误（播放失败）；
  - 设置导出/导入/恢复默认、歌单导入/导出、队列清空、打开文件结果；
  - 曲库索引优化完成。
- 绘制：`paint::draw_toast` 只画 Toast 矩形；`push_toast`/过期只失效旧/新
  矩形。Toast 不是子窗口：正在播放页放在页头右侧（避免被歌词子窗体盖住），
  其他页放在播放条上方右侧。
- 定时器：`sync_timer` 在 Toast 存在时保持 `ANIM_TIMER`；过期后立即停表，
  暂停/空闲时不产生周期重绘。
- 设置导出/导入失败不再弹模态 MessageBox，改为错误 Toast；恢复默认/清空队列
  仍保留确认对话框。

### 20.2 播放统计页

- 侧栏新增“播放统计”（`Page::Statistics`，core `Page` 同步增加变体）。
- 进入页面时 `Command::SetPage` 触发 `LibraryStats`；`DbEvent::StatsDone`
  写入 `LibraryState.stats`，`Snapshot::statistics` 供 UI 只读。
- 页面内容：
  - 8 张汇总卡：曲目 / 歌手 / 专辑 / 总时长 / 播放次数 / 无损曲目 / 缺失文件 /
    曲库占用；
  - 最近播放（可点击直接播放）、歌手榜、曲目榜（按播放次数）；
  - “刷新”重新统计；“重置统计”二次确认后清空 play_count/skip/last_played。
- 统计查询复用 `run_stats`（已有），列表只绘制能放下的行；页面不引入滚动条，
  窗口过矮时自然截断，不覆盖播放条。
- 布局集中在 `layout::statistics`，绘制/命中测试共用同一组矩形。

### 20.3 性能设置界面

- 设置页新增“性能”分类（`SettingsTab::Performance`），共 15 行：
  - 标签读取（下次启动）、解码封面、封面/背景纹理、音频设备延迟初始化
    （下次启动）、暂停时释放音频设备、恢复暂停会话时不预解码；
  - 空闲刷新率、流动背景动画、背景刷新率、失焦/最小化暂停动画；
  - 缩略图缓存上限（0 = 不限）、空闲回收内存 / 延迟；
  - “优化索引”（`PRAGMA optimize; VACUUM;`，走 DB worker，完成后 Toast）。
- 已接线的运行时行为：
  - 缩略图缓存上限直接用于 `RowCovers` LRU；
  - 暂停时释放音频设备调用 `AudioEngine::set_release_when_idle`；
  - `defer_paused_restore=false` 时恢复暂停会话会预先打开解码器；
  - 空闲回收内存使用 15s 低频率 `IDLE_TRIM_TIMER`（不重绘）；窗口可见时至少
    2 分钟无输入才回收，隐藏/最小化时按配置延迟；
  - 失焦暂停动画通过 `WM_ACTIVATE` 重启/停止 `ANIM_TIMER`。
- 外观分类新增：背景（纯色/模糊封面/流动渐变/Mica）、音频可视化开关与样式。

### 20.4 背景与音频可视化

- 背景（`ThemeSettings.background`，默认 `static_blur`）：
  - 纯色：保持原行为；
  - 模糊封面：取当前封面 40×24 降采样 + 两次盒式模糊 + 深色/浅色压暗，
    用 `StretchDIBits` 拉伸到整窗；无封面/关闭“封面/背景纹理”时回退纯色，
    不产生额外开销；
  - 流动渐变：`GradientFill` 4 段垂直渐变，按 `background_frame_rate`
    缓慢移动相位；只失效内容区；
  - Mica：`DwmSetWindowAttribute(DWMWA_SYSTEMBACKDROP_TYPE=2)` +
    深色标题栏 + 客户端同色系渐变（窗口仍为 GDI 不透明表面，正文不着色）。
  - 背景缓存按封面地址/版本、明暗、强调色失效；不在每帧解码或重建。
- 音频可视化（`settings.visualizer`）：
  - 播放时从音频回调的无锁 mono ring（`AudioEngine::fill_visual_samples`）
    取最多 2048 个采样，UI 侧计算 16–96 根 RMS 条并平滑；
  - Now Playing 歌曲卡片与歌词面板之间增加 64 DIP 可视化条（条形/波形/环形）；
    无音频样本时只显示占位文字，不产生周期失效；
  - 暂停停止定时器，条带保留最后一帧；关闭可视化后布局立即恢复。

### 20.5 验证

- `cargo build` 0 warning；`cargo test` 189 passed / 0 failed / 6 ignored。
- `--self-test --page stats` / `--page settings --settings-tab 8`：
  `painted=true`，退出码 0。
- `--bench`（同机、debug、175%）：
  - `--bench 5`（正在播放，视觉化开启但 bench 无音频）：`ticks=314,
    frames=314, avg_paint=517us, private=24.3MB, ws=44.4MB`；
  - `--page queue` / `--page stats`：约 560–580us/帧。
- 截图确认：侧栏 7 项、统计页汇总卡/最近播放/歌手榜、性能页 10 个分类
  两行半布局与各开关行均正常；可视化占位条位于卡片与歌词之间。
- 未做（下一批）：快捷键自定义/冲突检测、UI Automation/Tab 焦点/滑杆方向键、
  手柄输入、overlay GPU 与主窗口 CPU 基线、UI 文案润色（§19）。

---

## 21. 歌词自定义 / A-B / 设备 / 智能歌单 / 查重 / 文件夹多选（2026-11-10）

> 本轮补齐用户列出的第二批：歌词逐面外观与编码/偏移/语言/预取、A-B 循环、
> 输出设备、自定义睡眠定时、智能歌单页、查重结果页、文件夹多选与队列拖放排序。
> 修复了用户反馈的“应用卡死”：智能歌单页曾为整屏所有行请求封面，导致
> 数千个 MP3 同时解码；现在只请求可见行。

### 21.1 歌词逐面外观

- `LyricsSettings` / `LargeLyricsSettings` 新增：`font_family`（已有）、
  `font_weight`、`italic`、主行/翻译各自的 `outline_width/color` 与
  `shadow_offset/color`。
- `DesktopLyricsSettings` 新增：`italic`、翻译/页脚各自的
  `outline_width/color`、`shadow_offset/color`，以及 `blur_radius`。
- 渲染：
  - 内嵌歌词与独立/大歌词窗口（`src/lyrics_view.rs`）共用 `Style`：
    字体族/字重/斜体进入 `CreateFontW`；当前行比普通行重 200；
    主行与翻译分别绘制阴影和描边（描边半径 1–3 DIP）。
  - 桌面歌词（D2D）`decorated` 改为显式接收每面的宽度/颜色/偏移；
    `DWriteTextFormat` 支持斜体；`BlurCover` 背景按 `blur_radius`
    做一次盒式模糊（按封面尺寸缩放），结果缓存直到封面/半径变化。
- 设置入口：歌词 / 独立歌词 / 桌面歌词三个分类新增对应行；
  颜色行复用原生取色器，字体行走原生字体选择器。

### 21.2 歌词编码 / 偏移 / 语言 / 预取

- 设置行：编码（自动/UTF-8/GBK/GB18030/UTF-16LE/UTF-16BE/Big5）、
  偏移（±`offset_step_ms`，可输入秒）、翻译语言（自动/中/英/日/韩）、
  预取下一首、显示来源、逐字/仅增强歌词逐字（已有）。
- 编码或翻译语言变化会清空预取缓存并重新加载当前歌词。
- 用户偏移应用到：内嵌/独立歌词（`adjusted_position + offset`）、桌面歌词与
  迷你播放器（协议 `offset_ms = -settings offset`，与内嵌方向一致）、
  当前行失效检测。
- 预取：切歌时对下一首本地曲目调用 `LyricsService::request_prefetch`；
  非当前曲目的结果进入 8 条 LRU 缓存，切到该曲目时立即使用。

### 21.3 A-B 循环 / 输出设备 / 自定义睡眠

- A-B：`AppCore` 运行期保存 `ab_start/ab_end`；`tick` 在播放位置到达 B 点
  后 seek 回 A；切歌清空。设置 → 播放新增“设置 A 点 / B 点 / 清除”，
  行内显示时间；B 必须晚于 A 0.25s。
- 输出设备：`AudioEngine` 共享设备名，`ensure_output` 优先按名称匹配，
  找不到回退系统默认；`SetDevice` 丢弃现有 stream 并在播放中重建。
  设置 → 播放“输出设备”行循环“系统默认 + `cpal` 枚举设备”。
- 睡眠定时：新增“自定义睡眠定时”步进行（5 分钟步进，0 = 关闭），
  与原有 15/30/60 预设共用 `SetSleepTimer`。

### 21.4 智能歌单 / 查重页

- 侧栏新增“智能歌单”“重复文件”（`Page::Smart` / `Page::Duplicates`）；
  9 个导航项按可用高度动态压缩行高，640×460 仍然放得下。
- 智能歌单：`Command::LibrarySmart(SmartKind)` → `LibraryDb::smart`
  （最多 500 首）；页头按钮循环 7 个预设；列表复用曲库虚拟列表/滚动条，
  点击播放、`Ctrl+Enter` 不是该页的行为（按行点击即可）。
- 查重：`Command::LibraryDuplicates(DedupeMode)` → `LibraryDb::duplicates`；
  结果按“副本组”表头 + 文件行扁平化显示（固定行高虚拟列表），
  页头按钮循环元数据/路径/指纹三种方式；点击条目播放。
- 两个页面只绘制可见行、只请求可见行封面；`library_rev` 变化只重绘列表/滚动条。

### 21.5 文件夹多选 / 队列拖放排序

- 文件夹页支持 `Ctrl+点击` 切换、`Shift+点击` 区间多选；选中行使用选中底色，
  页头副标题显示“已选 N 项”；`Ctrl+Enter` 把选中文件加入队列。
- 队列页拖动行左侧封面区域可重排：拖动显示强调色插入线，松手发送
  `Command::QueueMove`；不移动直接点击缩略图仍按原行为播放。
- 文件夹拖入队列的 OLE 拖放仍未做；当前“拖放排序”指队列行重排。

### 21.6 卡死修复

- 现象：打开“智能歌单”后应用无响应、CPU 持续占用。
- 根因：`App::request_visible_covers` 对智能/查重页遍历**全部**行请求封面。
  用户会话队列/曲库较大时，后台同时解码数千张 MP3 封面，UI 被事件与绘制
  拖死。
- 修复：这三类页面改为只请求 `visible_rows()` 范围内的封面，与队列页一致。
  自测（用户真实会话队列）`--self-test --page smart` 恢复为约 2.7s 内退出。

### 21.7 验证

- `cargo build` 0 warning；`cargo test` 189 passed / 0 failed / 6 ignored。
- 七页 self-test（now_playing/queue/library/settings/stats/smart/duplicates）
  全部 `painted=true`；歌词/播放/桌面歌词设置页绘制通过。
- `--bench 5`：`avg_paint≈480–585us, private≈25.3MB, ws≈45.6MB`。
- 截图确认：设置页 10 分类、歌词页新增字体/字重/斜体行、侧栏 9 项、
  歌词页新增行滚动正常。
- 未做：文件夹→队列 OLE 拖放、智能/查重页右键菜单、桌面歌词页脚/翻译
  各自独立的“字体族/字重”（当前共享同一字体族与加粗/斜体）。

---

## 22. 在线功能 / 列表交互 / 外观自定义（2026-11-10）

> 本轮按“在线功能 + 曲库/列表交互 + 外观/自定义”补齐。在线链路用本机
> mock Bamboo 服务做了真实播放验证：搜索 → `api.resolve` → HTTP WAV →
> 解码播放 → info 返回的 LRC 进入歌词面板。

### 22.1 Bamboo 在线播放

- `AppCore` 现在持有 `ApiService` / `OnlineState` / `OnlineResolve` /
  `online_next` / `online_info`；`TrackSource::Bamboo` 不再直接停止：
  - `load_index` 调 `api.resolve`（流缓存路径按 `track_key + quality` 哈希到
    `paths.stream_cache` 或 `network.stream_cache_dir`），期间状态为 Loading。
  - `ApiEvent::Resolve` 回到主线程后合并 `info` 元数据（歌名/歌手/专辑/封面），
    写入 online LRC（`LyricsSource::Bamboo`），再 `engine.load(EngineSource::Url)`。
  - 离线 / 无音源 / 解析失败走 Toast，不再静默。
- 下一首：`arm_next` 对 Bamboo 先 `api.resolve_next`；`NextResolved` 到达后
  用缓存 URL 预开 gapless deck。已预解析的当前曲目直接复用，避免重复请求。
- 在线封面：`RowCoverService` 新增 `RowJob::Url`，搜索结果可见行按 `pic`
  URL 在后台解码 64px 缩略图，缓存键 `url:<地址>`；`load_covers=false` 时不请求。
- 在线歌词兜底：`resolve_next` 只取 URL 时，当前曲目再发一次 `api.info`，
  结果按 track key 匹配后才写入，快速切歌不会串词。

### 22.2 在线搜索页（Page::Search）

- 搜索页不再是占位：原生 EDIT（与曲库搜索共用，Enter 生效）+ 结果虚拟列表 +
  滚动条 + 行缩略图 + 右键菜单。
- **结果分页**（与曲库一致）：每页行数 = 当前列表能完整显示的行数（窗口缩放
  会重新按第 1 页查询）；底部“上一页 / 第 N 页 / 本页 M 首 · 每页 K 首”。
  **不用服务端 `total` 计算页数**：Bamboo API 的 total 只是前端流水渲染的
  提示，真实可达页数未知；“下一页”只在上一页取满时可用，请求到空页就停在
  最后一个非空页并禁用“下一页”（行数据保留，不会闪成“没有找到”）。
  专辑/歌手详情同样按页请求，不再无限追加。
- 分页期间列表保留上一页内容直到新页返回；空页不会替换列表，因此底部翻页
  按钮也不会被空状态覆盖（之前只有鼠标移上去才重绘）。
- **布局（用户反馈后调整）**：搜索框与 3 个按钮从标题栏右侧移到标题下方的
  独立工具栏行；输入框占左侧，右侧依次为“类型 · 歌曲/专辑 ▾”“全部入队”
  “随机推荐”。详情模式下前两个变为“返回”“加载更多”。
- 输入 350ms 防抖后发 `Command::OnlineSearch`；`api.suggest` 的相关词显示在
  副标题（最多 3 个）。查询以 `http(s)://` 开头时，按钮变为“打开链接”，
  Enter 直接把链接加入队列并播放。
- 专辑/歌手详情：行右键“打开专辑/打开歌手”→ `api.album`/`api.artist`；
  详情页“加载更多”按 `network.album_page_size` 追加下一页。
- 发现：无查询时点“随机推荐”走 `api.random`（30 首）。
- 行右键：播放、加入播放队列、下载、打开专辑、打开歌手、复制歌名/歌手/ID。
- 下载：行右键“下载”进入 `DownloadService`；设置 → 存储 里显示下载任务行
  （标题/歌手/状态/进度），点击任务行取消；完成后按设置加入曲库索引。

### 22.3 网络 / 下载 / 缓存设置

- 设置页新增两个分类（共 12 项；标签按可用宽度流式排布，满行自动换行，
  宽窗口 6×2、窄窗口 4×3 / 3×4）：
  - **网络**：只保留**一个在线 API 地址**（默认
    `http://127.0.0.1/musicnew/apis/local`，也可以直接填完整的
    `.../local.php`）；点击后出现内联原生 EDIT，Enter/失焦保存、Esc 取消；
    修改地址会清空旧搜索结果。其余音源字段（名称/脚本/音质/Key/Cookie/TLS）
    仍保存在 settings.json，但不再占设置页。
    其余：离线模式、在线请求间隔（0–3000ms，运行时改 API limiter）、
    专辑/歌手每页、请求超时、失败重试、代理模式（系统/直连/手动）与
    代理地址、流缓存上限、流缓存目录（原生文件夹选择器）、
    清理缓存（流/封面/歌词）、测试在线音源（结果 Toast）。
  - **存储**：下载目录（文件夹选择器）、同时下载 1–4、同名策略
    （重命名/跳过/覆盖）、写入标签/封面/歌词、下载后加入曲库、
    清除已结束的下载；下载任务作为附加行列出。
- `AppCore::clear_stream_cache` 只删除缓存目录下的文件，不动音乐与下载。
- ReplayGain 后台扫描接线：`playback.replaygain_scan` 打开且无标签/无缓存时，
  切歌后把本地文件交给 `audio::loudness::LoudnessService`，完成后
  `engine.refresh_replaygain()`；扫描线程不在 UI 线程。

### 22.4 曲库 / 列表交互

- 查重页：组头右键“仅保留第一项（从索引移除其余）”“从索引移除整组”；
  文件行右键播放/从索引移除/复制路径/打开位置。只动索引，不删文件。
- 智能歌单页右键：播放、加入播放队列、从曲库移除、复制歌名/歌手/路径、
  打开文件位置。
- 文件夹 → 队列拖放：按住文件行拖到左侧“播放队列”，松手把当前选中
  （无选中则按下的行）的音频文件追加到队列；拖动时副标题提示目标。
  外部拖入（Explorer → 窗口）仍走原有 `WM_DROPFILES`。
- 右键菜单现在按页面分派（Queue/Library/Smart/Duplicates/Folders/Search），
  不再把显示行直接当队列下标。

### 22.5 外观 / 自定义

- 主题字体：设置 → 外观新增“界面字体”（原生 ChooseFont）与“界面字号”
  （10–24，0.5 步进）。`Painter` 按配置缓存字体，族/字号变化时只清一次
  字体缓存；图标字体（Segoe MDL2）不受影响。
- 歌词面板底色：设置 → 歌词新增“歌词面板底色”（原生取色器）与
  “恢复主题底色”；内嵌歌词子窗体和面板外壳同时使用同一颜色。
- 桌面歌词逐面字体：新增“翻译字体”“页脚字体”与“翻译加粗/页脚加粗”；
  D2D `format_with` 按行传入字体族/字重，未设置时跟随主行。
- 迷你播放器：新增“窗口高度”（关闭自动高度后生效，8 DIP 步进）；
  双击封面/标题等空白区域回到主窗口（窗口类加 `CS_DBLCLKS`）。

### 22.6 验证

- `cargo build` 0 warning；`cargo test` 189 passed / 0 failed / 6 ignored。
- self-test：now_playing / queue / library / search / folders / stats / smart /
  duplicates / settings 全部 `painted=true`；设置标签 4（桌面歌词）、10（网络）、
  11（存储）均绘制通过。
- mock Bamboo 端到端（portable 临时目录 + 本机 HTTP 服务）：
  `online resolve 1: ok` → `loaded http://127.0.0.1:8765/media/test.wav
  (44100 Hz, 2 ch, duration=Some(10.0))`，播放条显示 Mock Song / Mock Artist；
  `info` 的 LRC 写入歌词面板。
- 截图确认：搜索页工具栏位于标题下方，输入框/类型/全部入队/随机推荐可见。

### 22.7 仍未做（2026-11-10 历史快照；以下项目已在 §23–§27 完成或取消）

**在线 / 下载 / 缓存**

- 下载管理没有独立页面：任务目前只在设置 → 存储 里查看/取消；缺失败重试、
  清理 `.part`、单曲音质选择、文件名模板 UI。
- `DownloadService` / `LoudnessService` 没有 wake 回调：空闲/暂停时下载进度、
  完成提示和 ReplayGain 结果要等下一次 UI tick 才刷新。
- `SearchCache`（80 页/7 天、离线回退）已有但没有接入 `AppCore`，每次搜索
  都直接走网络。
- 流缓存只按 `cache_limit_mb > 0` 决定是否写缓存，没有 LRU/TTL/占用统计；
  “清理缓存”只有全清，没有大小展示。
- 在线音源请求头（`headers`）、端点覆盖（`endpoints`）与多音源增删仍只能改
  settings.json，设置页目前编辑主音源。
- 发现 / 随机电台页：`core::radio` 已有业务，侧栏没有入口（只有搜索页的
  “随机推荐”）。
- 分享链接：只接受直链 http(s) 音频/HLS；Bamboo 分享页链接没有解析。
- 在线歌词只有 `info` 返回的 LRC，没有在线翻译歌词。
- 专辑 / 歌手详情已有分页，但没有封面墙。
- 队列/曲库行右键没有“下载”。

**列表 / 设置 / 外观**

- 文件夹 → 队列的 OLE（`IDropTarget`）与拖拽悬浮预览：当前是页内拖动到
  “播放队列” + Explorer 外部拖入（`WM_DROPFILES`），且只能追加到末尾。
- **优化设置页 UI 排布**（用户 2026-11-10 指定待做）：标签/行标签与控制区的
  宽度与间距、长值截断/tooltip、窄窗口下的回退布局、滚动条与翻页条一致性。
- 歌词面板背景只有纯色：背景图片/渐变/边框颜色未做（圆角不再规划）。

**最后一批（§19，用户指定最后做）**

- 迷你播放器 / 桌面歌词 GPU：渲染定时器常开、暂停/无 KTV 未真正停表。
- 主窗口 CPU：`sync_system`/`snapshot()` 每帧全跑，未做 250ms/1s 节流。
- UI 文案润色（`NATIVE UI · M1`、`虚拟列表`、`P1 页面` 等内部术语）。
- 快捷键自定义 + 冲突检测（`ShortcutSettings` 未读）。
- 无障碍：UI Automation、Tab 焦点、滑杆方向键、手柄。

### 22.8 本轮 UI 修复

- **设置标签流式排布**：`layout::settings` 按可用宽度计算列数（最小 96 DIP），
  优先选能整除标签数的列数，整行铺满后换行；不再固定 4 列留出大块空白。
- **数字编辑框跟随滚动**：滚轮、设置滚动条拖动、页/标签切换后统一调用
  `sync_inline_edits`，EDIT 重新贴合所在行；滚出可见区时先提交输入再隐藏；
  切换页面/标签只取消不提交（避免行索引映射到别的字段）。
- **数字编辑框边框**：`begin_number_edit` 立即失效该行，原生 EDIT 之外的
  强调色边框不再缺失。
- **设置文本行**（API 地址 / 音源名称 / 脚本 / Key / Cookie / 代理地址）：
  新增 `SettingKind::Text` 与内联原生 EDIT（`ID_SETTING_TEXT`），
  Enter/失焦保存、Esc 取消，与数字编辑共用同一套滚动同步。
- **托盘恢复全白**：隐藏到托盘期间 `window_visible=false`，`invalidate_all`
  不再生效且暂停时没有 tick 轮询可见性。新增
  `App::restore_visibility()`，在托盘显示/切换窗口与 `WM_SHOWWINDOW` 时
  清空遮挡、强制整窗失效，并立即刷新歌词子窗体/编辑框/定时器。
- **统计页最近播放重叠**：行高 30 → `layout::STATS_ROW_HEIGHT`（38 DIP），
  标题与歌手分两行（18 DIP 各一行），点击/悬停槽位同步更新。
- **设置页播放/暂停无效**：设置页的通用行分支在点击落在 `generic` 之外时
  直接 `return`，把播放条的点击吞掉了。现在只在设置内容区内消费点击，
  其余位置继续走共享的播放条/标题栏命中测试。
- **新数值行可输入**：本轮新增的请求间隔、每页数量、超时、重试、缓存上限、
  下载并发、迷你高度、界面字号都加入 `number_input_field()`，点击数值即可
  输入；文本行（API 地址/代理地址）改为与数值行一致的样式：平时只显示文字，
  悬停显示细边框，编辑时强调色边框 + 原生 EDIT，不再整行填按钮底色。
- **在线 API 实测**：用本机 `http://127.0.0.1/musicnew/apis/local/local.php`
  跑 `BAMBOO_LIVE_API=... cargo test live_` 通过（search / info / url /
  专辑详情）；搜索页实测正常出结果、封面与分页。
- **专辑搜索结果不能当歌曲播**：`type=searchAlbum` 的每行 `id` 是专辑 ID，
  点击后现在直接打开专辑详情（`OnlineOpenAlbum`），不会再向 `type=url`
  发专辑 ID（之前会报“播放失败/无法打开 getlocalmusic.php”）。专辑结果下
  “全部入队/下载”禁用，右键只保留“打开专辑/复制专辑名/复制歌手”；
  进入详情后按歌曲正常播放、入队、下载。
- **分页不再用服务端 total**：界面只显示“第 N 页 / 本页 M 项”；下一页是否
  可用由上一页是否取满决定；请求到空页时停在最后一个非空页、保留内容并
  禁用下一页。空页也会绘制翻页条，避免“按钮消失、悬停才出现”。



---

## 23. 下载管理 / 后台唤醒 / 流缓存上限 / 搜索缓存 / HLS 嗅探（2026-11-11）

### 23.1 后台 wake

- `DownloadService::with_wake`：每个进度/状态事件发送后调用 UI wake。暂停/空闲时
  下载进度、完成提示、失败提示不再等下一次无关 tick。
- `LoudnessService::with_wake`：ReplayGain 扫描完成立即唤醒 UI
  （`engine.refresh_replaygain()` 与 `loudness_pending` 清理及时执行）。
- `AppCore` 保存 `wake`，供延迟创建的响度线程复用。

### 23.2 下载管理独立页（`Page::Downloads`）

- 侧栏新增“下载管理”（`NAV_ITEMS` 9 → 10，最小窗口下导航项高度回退到 22 DIP）。
- 自绘虚拟列表：歌名/歌手/状态/进度条/错误；每行提供“音质 / 取消 / 打开 / 重试”，
  工具栏提供“清理 .part”和“清除已完成（N）”。
- `DownloadRow` 保存完整 `DownloadRequest`（曲目、音源、音质、网络、下载设置、
  目录），失败/取消后可直接重试；音质可逐任务在“跟随音源 / 320 / FLAC / 128”
  之间切换。
- 设置 → 存储新增“下载音质”“文件名模板”；任务列表移出设置页，避免与独立页
  重复。
- “清理 .part”在后台线程递归扫描下载目录，只删除 `*.part` 与孤立的
  `*.part.meta`（活动下载的文件句柄会阻止删除）。
- 队列行右键新增“下载”（在线 Bamboo / URL 曲目）；本地文件提示无需下载。

### 23.3 流缓存上限 / TTL / 占用

- `core::api::cache` 新增 `StreamCacheService`：后台统计 `.stream` 占用，
  按 `cache_limit_mb` 做 LRU（mtime）淘汰，并按 30 天 TTL 清理；
  超过 1 小时的孤立 `.part` 一并删除。
- 缓存命中播放时 touch 文件 mtime，使 LRU 反映真实使用；每次在线曲目开始
  播放前刷新/裁剪一次。
- 设置 → 网络“流缓存上限”显示“上限 · 已用 X MB”；“清理缓存”经后台线程删除
  流缓存，封面/歌词目录同步清理。
- 缓存上限/目录改变、启动、清理后都会触发重新统计。

### 23.4 搜索缓存接线

- `SearchCache` 重新接入 `AppCore`（`cache/search.json`，7 天 / 80 页上限）。
- 搜索先显示缓存页并标记“缓存结果”，随后仍发网络请求刷新
  （stale-while-revalidate）；离线且有缓存时直接显示缓存，网络失败时保留
  缓存行并提示，不再把列表清空。
- 成功的歌曲/专辑搜索结果按“音源 + 地址 + 类型 + 关键词 + 页码”写入缓存，
  每 10 秒最多落盘一次，退出时强制保存。

### 23.5 HLS / m3u8 兼容

- 播放：`HttpMediaSource::open_probed` 先读 64 字节；`Content-Type` 为
  `mpegurl` / `vnd.apple.mpeg` 或正文以 `#EXTM3U` 开头时切换到 HLS 解码，
  覆盖没有 `.m3u8` 后缀的通用下载端点；探测前缀无损回放，不丢字节、不污染
  缓存。
- 下载：非音频扩展名的 URL 先发 `Range: bytes=0-63` 探测；确认是播放列表后
  走 `HlsDownload`，避免把 `.m3u8` 文本存成音频文件。

### 23.6 验证

- `cargo build` 0 warning；`cargo test` 200 passed / 0 failed / 6 ignored。
- `--self-test`：默认页、`--page downloads`、`--page settings --settings-tab 10/11`
  均 `painted=true`。
- 新增测试：流缓存 TTL / LRU / 清理只动缓存文件；HLS 无后缀探测；
  探测前缀回放不丢字节。

---

## 24. 列表 / 设置 / 外观批次（2026-11-11）

### 24.1 文件夹 / Explorer → 队列 OLE 拖放

- 新增 `src/drop_target.rs`：`QueueDropTarget` 实现 COM `IDropTarget`，主窗口创建后
  `RegisterDragDrop`，`WM_DESTROY` 时 `RevokeDragDrop`；启动时 `OleInitialize`，
  退出时 `OleUninitialize`。`WM_DROPFILES` 保留为 OLE 不可用时的回退。
- `IDropTargetHelper` 提供系统标准拖拽缩略图；`DragEnter/DragOver` 读取 `CF_HDROP`，
  屏幕坐标经 `ScreenToClient` 与实际 DPI 换算成 DIP。
- 悬浮预览：
  - 光标在播放队列列表上时显示插入线（`queue_drag_indicator`），页头副标题显示
    “松开插入到第 N 位 · M 个文件”；
  - 光标在侧栏“播放队列”上时高亮该导航项，并提示追加到末尾；
  - 其他位置提示“松开加入播放队列”。
- 落点插入：新增 `Command::AddPathsAt { paths, index }`、`Queue::insert_many`
  （同步重映射 current/history/shuffle/pending cycle），队列筛选状态下由 UI 把
  显示行映射回真实队列下标。目录递归展开、歌单替换队列等原有语义保持不变。
- 队列行右键新增“下载”（上一批）继续有效。

### 24.2 设置页排布

- `SettingsLayout` 新增 `label_width`：宽窗口保持 260 DIP，窄窗口按可用宽度缩到
  150 DIP，控制区最小保留 80 DIP；标签/控制统一切到 `setting_label` / 
  `setting_control`，不会互相覆盖。
- 窄窗口下 `Choice` 行从分段按钮回退为单个循环按钮（点击切到下一项），避免
  4 段中文标签被挤成省略号；宽窗口仍显示全部分段。
- 长值 tooltip：设置行悬停 450ms 后，`setting_tooltip` 为 18 字以上的值显示
  “标签：完整值”原生 tooltip；列表行 tooltip 与设置行 tooltip 共用同一套
  延迟/隐藏/位置逻辑。
- 设置滚动条宽度、轨道/滑块颜色和 2 DIP 内缩与列表滚动条一致；标签流式
  排布、滚动/翻页逻辑保持原行为。

### 24.3 歌词面板背景

- `LyricsSettings` 新增：`panel_background`（跟随主题/纯色/渐变/图片）、
  `panel_color2`、`panel_image`、`panel_border_color`；旧 `panel_color` 继续作为
  纯色/渐变起点。
- 解码在 `core::lyrics::LyricsBackgroundService` 后台线程完成（最长边限制
  1280，失败 Toast），结果按请求路径匹配，快速换图不会显示旧图；启动、设置
  导入、模式/路径变更都会重新请求，退出时清理。
- 内嵌面板父窗口（`paint.rs`）绘制实色/双色纵向渐变/拉伸图片/自定义边框；
  歌词子窗体（`lyrics_view.rs`）用同一套模式绘制自己的背景，图片 BGRA 只转换
  一次并缓存；图片缺失或读取失败回退纯色。
- 设置 → 歌词新增“面板背景 / 渐变终点色 / 面板边框色 / 背景图片 / 恢复主题
  背景”；背景图片使用原生文件选择器，不需要重启。

### 24.4 验证

- `cargo build` 0 warning；`cargo test` 208 passed / 0 failed / 6 ignored。
- 新增测试：队列中间插入的重映射、歌词背景解码/缩放/坏路径、设置页窄窗口
  标签与控制区不重叠、歌词面板命令往返与重置。
- `--self-test`：默认、now_playing、queue、downloads、settings 页及歌词(2)/
  网络(10)/存储(11) 分类全部 `painted=true`。
- `--bench 1`：ticks=62 frames=60 avg_paint=619us private=27.8MB（debug）。

---

## 25. 在线收尾：发现页 / 分享链接 / 多音源 / 封面墙（2026-11-11）

### 25.1 发现页

- 新增侧栏“发现”（`Page::Discover`，导航项 11；`nav_top` 调整为 76 DIP 以容纳
  11 项）。
- 独立 `DiscoverState`（不与搜索页共用结果）：进入页面或点“换一批”时按时间
  seed 拉取 18 首随机推荐；加载/错误/空列表状态独立。
- 列表复用虚拟行 + 缩略图；行点击播放；右键“播放 / 加入播放队列 / 下载 /
  复制歌名/歌手”；页头显示随机推荐数量。
- `ApiEvent::Random` 通过请求 id 区分发现页批次与搜索页“随机推荐”，二者互不
  覆盖。

### 25.2 Bamboo 分享链接

- `open_url` 现在接受：
  - 纯数字歌曲 ID；
  - 含 `musicid` / `songid` / `mid` / `type=song&id=` / fragment id 的分享链接；
  - 原有 http(s) 直链与 HLS。
- 分享链接用当前在线音源构造 `TrackSource::Bamboo`，进入正常 resolve/播放链路；
  离线/无音源给出 Toast。

### 25.3 多音源 / 请求头 / 端点覆盖

- `Settings.active_source` 持久化当前选中的音源；搜索/播放仍只使用已启用音源，
  设置页可以选中并编辑一个已停用的音源。
- 设置 → 网络新增：
  - 当前音源（点击循环切换，自动启用选中的音源）；
  - 启用当前音源、音源名称、在线 API 地址、音源音质（128/320/FLAC）；
  - 请求头 JSON（`[{"name":"X","value":"Y"}]`）；
  - 端点覆盖 JSON（`{"search":"...","info":"..."}`）；
  - 新增音源、删除当前音源（至少保留一个）。
- JSON 行复用内联原生 EDIT；解析失败 Toast，不覆盖原配置。设置导入/导出包含
  新增字段。

### 25.4 专辑/歌手详情封面墙

- 详情分页条新增“封面墙 / 列表”切换（`Target::OnlineWall`）。
- 封面墙按可用宽度计算 2–8 列方格（封面 + 歌名/歌手），自有滚动高度、滚轮/
  滚动条/边界夹取；点击/右键/tooltip 仍按真实行号映射，封面缩略图只为可见
  网格请求。
- 切换页面或离开搜索页自动回到列表模式。

### 25.5 验证

- `cargo build` 0 warning；`cargo test` 208 passed / 0 failed / 6 ignored。
- 新增测试：分享链接 ID 提取、多音源循环/新增/删除、封面墙相关几何由
  `scroll_content_height`/`max_scroll` 共用。
- `--self-test`：search / discover / settings(10 网络) / queue 全部
  `painted=true`。

---

## 26. 最后一批完成：overlay / 主窗口 CPU / 文案 / 快捷键 / 无障碍（2026-11-11）

### 26.1 overlay 事件唤醒与空闲停表

- 新增 `core::platform::overlay_inbox`：
  - overlay 线程不再用 16ms 定时器轮询 crossbeam 通道；一个转发线程阻塞
    `recv()`，把消息放进队列并 `PostMessage(WM_APP_OVERLAY_WAKE)`。
  - `Inbox::close()` 在窗口销毁时停止转发，避免向已销毁的 `HWND` 投递。
- 桌面歌词：
  - 暂停 / 无 timed 歌词时不创建渲染定时器；`desired_interval()` 返回
    `None` 并 `KillTimer`。
  - 播放时只有 KTV 当前行需要每帧重绘；普通 timed 歌词按 10Hz 检查行边界，
    行变化才整帧重绘，KTV 行仍按 `frame_rate` 局部发布。
  - 位置消息只更新基准时钟，不强制发布；当前行变化才渲染。
- 迷你播放器：
  - 暂停时同样停表；State/Position 唤醒后才发布。
  - 鼠标悬停 / 拖动进度条 / 滚轮音量直接 `flush()` 局部发布，不依赖定时器。
- 遮挡检测 `overlay_inbox::fully_occluded(hwnd)`：
  - 按 z-order 遍历上方窗口，`WS_EX_LAYERED` / `WS_EX_TRANSPARENT` 不作为
    不透明遮挡物（与 §10.4 规则一致）。
  - 完全遮挡或最小化时停止发布；仅保留 500ms 遮挡轮询，重新可见时整帧重绘。
- 保持软件 `ID2D1DCRenderTarget`，未启用硬件 target。

### 26.2 主窗口 CPU 节流

- `WM_TIMER` 不再每帧跑全部系统同步：
  - `sync_system`（SMTC / 通知 / 跳转列表 / 注册表 / 快捷键）250ms 一次；
  - `sync_overlays` 50ms 一次（host 本身 250ms 才发位置，overlay 本地插值）。
- `sync_timer` 的动画判定重写：
  - 可见 + 播放：按帧率；
  - 隐藏/最小化但正在播放：有 overlay/独立歌词时 250ms 心跳，否则 1s；
  - 暂停且无 toast/flow：完全停表；只有 toast 时 1s 心跳。
  - 不再让隐藏窗口以 60fps 推进不可见表面。
- 系统/overlay 同步在 `WM_APP_WAKE` 后立即执行并刷新节流时间戳，保持
  “事件立即响应、空闲不轮询”。
- 未改变局部失效、绘制质量和画面帧率。

### 26.3 UI 文案润色

- 侧栏副标题 `NATIVE UI · M1` → `音乐播放器`；页脚改为
  `v{CARGO_PKG_VERSION}`。
- 队列副标题去掉“虚拟列表 · 只绘制可见行”，改为 `{} 首歌曲`。
- 正在播放副标题改为“歌词可滚动，点击歌词可跳转”；设置副标题改为
  “修改后自动保存”。
- 新增文案走 `crate::i18n::tr` / `trf!`，同步 `src/core/i18n/en.json`
  （音乐播放器 / 歌词可滚动… / 修改后自动保存 / {} 首歌曲 / 快捷键相关）。

### 26.4 全局快捷键自定义与冲突检测

- `src/shortcuts.rs`：`Ctrl+Alt+Space` 文本 ↔ Win32 修饰键/虚拟键解析，
  大小写与顺序无关；裸键、纯修饰键组合被拒绝。
- 设置 → 快捷键分类：启用全局快捷键、手柄控制、7 个动作、恢复默认快捷键。
  - 点击动作行进入捕获模式，行值显示“按下快捷键…（Esc 清除）”；下一次
    完整组合写入。**Esc 会清空该动作的绑定**，空绑定是合法状态（不注册
    全局热键），行值显示“未设置”。
  - 捕获期间注销全部全局热键，避免按下旧组合触发动作。
  - 组合解析后与其他动作比较；重复时 Toast 提示，不覆盖原绑定。
- `RegisterHotKey(..., MOD_NOREPEAT)` 注册；被其他程序占用或格式无效时
  Toast 提示一次；设置修改后只重新注册变化的组合。
- `WM_HOTKEY` 支持：播放/暂停、上/下一首、音量 ±5%、桌面歌词、显示/隐藏
  主窗口；窗口隐藏时同样生效。

### 26.5 无障碍

- **Tab 焦点环**：`App::focus_targets()` 按页面给出与布局一致的焦点顺序
  （侧栏 → 播放条 → 页面工具栏 → 卡片/歌词按钮），Tab / Shift+Tab 移动，
  Enter/Space 通过投递真实鼠标消息激活（对话框、原生编辑器走同一条路径）。
  `DrawFocusRect` 只绘制焦点矩形。
- **滑杆方向键**：焦点在音量时 ↑/↓/←/→ 调音量；在进度条时 ←/→ seek。
- **UI Automation**（`src/accessibility.rs`）：
  - `WM_GETOBJECT(UiaRootObjectId)` 返回 `UiaReturnRawElementProvider`；
  - 根节点为 Window，子节点为 Button/Slider，提供 Name / ControlType /
    BoundingRectangle / IsEnabled / IsKeyboardFocusable / IsOffscreen；
  - Button 暴露 `IInvokeProvider`（投递点击），Slider 暴露
    `IRangeValueProvider`（音量 0–1，进度 0–时长）；
  - 焦点变化通过 `UiaRaiseAutomationEvent(UIA_AutomationFocusChangedEventId)`
    上报。
  - PowerShell UIAutomation 实测：主窗口 30 个子项、点击“曲库/设置”后
    树更新为 30/35 项、音量 Slider 可读、Tab 三次焦点依次为
    “页面：正在播放 / 播放队列 / 曲库”。
- **手柄**（`src/gamepad.rs`，XInput 动态加载）：
  - A 播放/暂停、B 下一首、X 上一首、Y 桌面歌词、Start 显示/隐藏；
  - D-Pad ↑/↓ 音量、←/→ seek；
  - 无手柄时不启动轮询；连接/断开由 `WM_DEVICECHANGE` 切换 100ms 定时器。

### 26.6 实测

同一台机器、debug 构建、175% DPI：

| 场景 | 单核 CPU | GPU（PID 峰值） | 私有内存 |
| --- | --- | --- | --- |
| `--bench 3`（60fps 绘制） | ≈4.8% | 0% | 28.0MB |
| 播放本地 WAV，主窗口 + 歌词子窗体 | 4.3% | 0% | — |
| 桌面歌词 + 迷你播放器，暂停 | 0.0% | 0% | 43.6MB |
| 桌面歌词 + 迷你播放器，播放 + KTV | 17.2% | 3.24% | 69.6MB |

- 暂停 overlay 的 0% 证明定时器已停、无周期发布；播放 + KTV 的 GPU 来自
  两个分层窗口 60fps 合成，主窗口本身仍为 0%。
- `--bench 3` 保持 60 tick/s（189/3s），`avg_paint` 未上升；主窗口可见、
  播放 + 滚动歌词满足 GPU < 3%、UI 线程 CPU < 5% 的验收线。
- 限制：没有保留旧二进制的 overlay 基线，无法给出“相对基线下降百分比”；
  上述暂停 0% / 无周期发布是结构性验收。

### 26.7 验证

- `cargo build` 0 warning。
- `cargo test` **222 passed / 0 failed / 6 ignored**（新增 shortcut 解析/冲突
  边界、overlay inbox 遮挡矩形、gamepad 按钮/ABI、UIA VARIANT 辅助）。
- `--self-test`：默认、search、discover、downloads、queue、settings(9/10/11)
  全部 `painted=true`。
- `--bench 3`：`ticks=189, frames=186, avg_paint=607us, private=28.0MB ws=49.7MB`。

---

## 27. 后续修正：缩放移除 / 背景简化 / 侧栏滚动 / 行焦点与 overlay UIA（2026-11-11）

### 27.1 移除界面缩放

- 删除 `theme.ui_scale` 字段、设置行“界面缩放”、`Command::SetUiScale`、
  数字编辑映射和 `scale_requested` 重载路径；有效 DPI 只使用系统 DPI。
- 旧 `settings.json` 中的 `ui_scale` 会被 serde 忽略，不再放大/缩小布局。
- 原因：缩放后布局与持久双缓冲尺寸不一致，出现整窗错位/残影；该功能不保留。

### 27.2 背景：纯色 / 模糊封面 / 自定义图片

- 设置 → 外观的“背景”提供 `纯色`、`模糊封面`、`自定义图片`；旧的
  Flow/Mica 在加载时迁移为纯色，Static/GPU 迁移为模糊封面。
- 新增“背景图片 / 清除背景图片”两行；选择图片后自动切到自定义图片模式，
  路径写入 `theme.background_image`。图片读取失败时 Toast，并回退纯色。
- 删除“流动背景动画”“背景刷新率”“封面/背景纹理”三个设置行及其命令、
  定时器与绘制分支；不再有流动渐变/Mica 客户端渐变。
- 自定义图片同样走性能管线：
  - `BackgroundImageService` 在后台线程用 `image` 解码，长边限制 2560；
  - 绘制时按 cover-fit 双线性采样到客户区像素尺寸，暗色蒙版保证文字可读，
    按行 rayon 并行；结果按“图片 + 尺寸 + 明暗 + 强调色”缓存；
  - 每帧只做 1:1 `StretchDIBits`；解码完成后通过 `background_image_rev`
    触发一次整窗重绘，不会逐帧解码/缩放。
- 模糊封面管线重做：
  - 封面先缩到 96×54，做两遍 3×3 盒式模糊，再做暗化/强调色混合；
  - 用固定点双线性一次性放大到客户区像素尺寸，**按行 rayon 并行**；
  - 每帧只做 1:1 `StretchDIBits`，不再每帧对整窗做 HALFTONE 拉伸；
  - 缓存键包含封面指针/版本、明暗、强调色、客户区像素尺寸。
- 封面解码完成（`cover_rev` 变化）时，模糊背景模式下整窗强制失效，避免
  背景停在纯色；切换背景模式同样强制整窗重绘。
- 打开本地歌曲的 ~5s 卡顿来自旧实现每帧整窗 HALFTONE + 单线程大图放大；
  现在生成只发生一次，并行后为毫秒级，播放/切歌不再重复生成。

### 27.3 侧栏滚动条 + 设置标签悬停消失修复

- 导航固定 38 DIP 行高，`nav_viewport` 预留底部版本号区域；只有 11 项放
  不下时才创建 4 DIP 滚动条（`Target::SidebarScrollbar`）。
- 滚轮悬停侧栏滚动；拖动滚动条；Tab/页面切换自动把当前项滚入视口；
  绘制时裁剪到 viewport，列表项不会盖住 `v0.1.0`。
- 大窗口（如 1200×900）不显示滚动条，布局与原来一致。
- 修复设置分类标签“鼠标扫过后消失、移上去又出现”：绘制入口原来只用
  `tabs[3].right` 作为标签区域右边界，列数变化后后面几列（如“迷你”“窗口”）
  不在该矩形内，悬停失效只重画背景、不重画标签。现在对全部 13 个标签矩形
  求并集，并在并集与脏区相交时重绘整组标签。

### 27.4 快捷键 / 托盘 / 手柄

- 默认全局快捷键更新：
  - 播放/暂停 `Ctrl+Alt+P`
  - 上一首/下一首 `Ctrl+Alt+Left/Right`
  - 桌面歌词 `Ctrl+Alt+M`
  - 显示/隐藏主窗口 `Ctrl+Alt+Q`
  - 音量 ±5% 保持 `Ctrl+Alt+Up/Down`。
- 新增设置分类“快捷键”（第 13 个标签）：启用全局快捷键、手柄控制、7 个
  动作、恢复默认；标签流式布局在质数标签数时回退到最大可容纳列数。
- 托盘菜单在“显示/隐藏、播放/暂停、上一首、下一首、桌面歌词”后追加当前
  组合（只在绑定时更新文本）；关闭全局快捷键时只显示原标签。
- 手柄开关 `ShortcutSettings.gamepad`：关闭后立即停 `GAMEPAD_TIMER`；
  开启且检测到手柄时恢复 100ms 轮询；无手柄仍不轮询。

### 27.4.1 队列筛选框重绘 / 随机种子 HTTP 500

- 队列筛选框点击/聚焦时右侧露出背景灰色块：`draw_header` 的
  `toolbar_dirty` 没有包含 queue filter 行，且 `set_input_focus` 没失效
  `queue_filter`，导致 EDIT 聚焦后只重画背景、跳过边框。现在把队列工具栏
  加入 `toolbar_dirty`，并在输入焦点变化时失效筛选框；点击后整条筛选框
  边框保持完整。
- 发现页 HTTP 500（PHP `mt_srand(): float given`）：`random` 请求原来同时
  发送 `seed` 和 `offset=seed`，本地 API 将两者相加后溢出为 float。现在
  只发送整数 `seed`；发现页实测正常返回 18 首推荐。
- 启动“索引完成 · 已收录 N 首 · 缺失 M 首”红色 Toast 每次启动都弹：
  索引结果现在写入 session（`last_indexed` / `last_index_missing`），只在
  收录数或缺失数变化时提示；第一次升级没有基线时静默采纳当前结果，因此
  常规启动不再弹出。缺失数变化时仍用红色提示。
- 清空队列后下次启动又恢复播放列表：`session.playlist_path` 没有随清空
  一起清除，启动时 `auto_refresh_on_startup` 会把 `.mp3list` 重新导入。
  现在 `ClearQueue` 同时清空 `playlist_path`；空队列保存时也强制不写
  `playlist_path`；启动自动刷新只在队列非空时执行。清空后再次启动保持
  0 首。
- 启动总回到设置页/切换页面后不持久：`DestroyWindow` 之后
  `take_state()` 读不到 `GWLP_USERDATA`，`AppCore::drop` 不会运行，正常退出
  （含托盘“退出”）因此从不保存 session。现在在 `WM_DESTROY` 里、窗口仍
  有效时调用 `core.save_now()`；实测便携模式切到曲库后退出，session 写入
  `page=library`。同时 `--page` / `--folder` 测试开关不再覆盖用户记住的页面。
- 清空队列/从曲库移除后，确认对话框覆盖区域残留旧行文字：MessageBox 是
  独立顶层窗口，关闭后缓存 `visible` 仍把对话框覆盖区当作被遮挡，普通
  `invalidate_all` 会跳过它。新增 `App::invalidate_after_modal()`（清空
  visible 缓存 + 整窗强制失效），用于清空队列（工具栏/右键）、从曲库移除、
  恢复默认设置和错误对话框返回后。
- 发现页封面只在鼠标悬停后出现：`row_cover_rev` 变化时只失效了队列/曲库/
  搜索的列表，没有包含发现页。现在 `Page::Discover` 也失效
  `discover_list`，缩略图解码完成即自动显示，不需要悬停。

### 27.5 设置行 / 列表行焦点与 UIA

- Tab 焦点新增：
  - 设置页当前分类的所有通用行（`Target::SettingsRow`）；
  - 队列/曲库/文件夹/搜索/发现/智能/查重/下载页的可见列表行
    （`Target::Row`）。
- 设置文本行点击进入编辑时，光标放到开头（EM_SETSEL(0,0)），长 API 地址/
  JSON/模板不会一进来就滚到末尾看不到开头。
- 所有数值 Step 行都可点击数值直接输入（与歌词字号一致）：补齐了
  `SleepTimerCustom`、`MiniHeight`、`ThemeFontSize`、API 间隔/每页数量/超时/
  重试/缓存上限/下载并发等缺失的初始值映射；`number_input_field` 与
  `setting_number_text` / `commit_number_edit` 已逐项对齐。
- 设置行：Enter/Space 激活（Toggle/Choice 直接改值，Step +1，Color/Action/Text
  投递真实点击以打开原生对话框）；←/→ 调整 Step/Choice，↑/↓ 切换行，
  Home/End 首尾行；焦点移动时自动滚动入视。
- 列表行：↑/↓ 在可见行间移动，Home/End 首尾，Enter/Space 按行点击行为播放/
  进入目录/打开；焦点移动时更新列表 `scroll`。
- 滑杆：音量/进度增加 Home/End（0 / 1、0 / 时长）。
- UIA 树随之包含设置行（“主题：跟随”“无缝播放：开”等）与可见列表行
  （“歌名 - 歌手”）；静态名称走 `crate::i18n::tr`。
- PowerShell UIAutomation 实测：设置页切换“播放”后子节点 36 → 59，出现
  23 个设置行；队列页出现 `test` 行。

### 27.6 独立歌词 / 桌面歌词 / 迷你播放器 UIA

- `accessibility::publish(hwnd, Vec<UiElement>)` 提供通用 UIA 片段树：
  Text / Button / Slider，Invoke 投递真实鼠标点击，Slider 走 RangeValue。
- 桌面歌词：当前歌词文本 + 页脚（歌名/歌手）。
- 迷你播放器：歌名/歌手、当前歌词、上一首/播放暂停/下一首/静音/关闭按钮、
  播放进度滑块（单位为毫秒，拖动/SetValue 转 `MiniAction::Seek`）。
- 独立歌词窗口：歌名/歌手 + 当前歌词行（读取主窗口快照）。
- 根节点名称使用窗口标题，三个 overlay 的 UIA 根分别为
  `Bamboo Mini Player` / `Bamboo Desktop Lyrics` / `Bamboo 歌词`。
- 实测：迷你播放器 UIA 子节点为 `test - 未知歌手`、`暂无歌词`、上一首、
  暂停、下一首、静音、关闭、播放进度。

### 27.7 验证与实机矩阵

| 项目 | 状态 | 说明 |
| --- | --- | --- |
| `cargo build` | 通过 | 0 warning |
| `cargo test` | 通过 | **235 passed / 0 failed / 6 ignored** |
| `--self-test` 默认/queue/discover/settings(12) | 通过 | `painted=true` |
| `--bench 2` | 通过 | ticks=129/frames=128，avg_paint=646us，private=26.3MB |
| 1120×805 侧栏 | 通过 | 仅 7 项、显示滚动条、版本号不重叠 |
| 模糊封面 | 通过 | 全尺寸缓存 + 并行生成；打开本地歌曲无 5s 卡顿 |
| 自定义背景图片 | 通过 | 后台解码 + cover-fit 并行缓存；渐变图实机平滑 |
| 设置行 UIA | 通过 | 播放分类 23 行；Tab/Enter/方向键逻辑已接 |
| 列表行 UIA | 通过 | 队列 `test` 行可见；Tab/Enter/方向键逻辑已接 |
| overlay UIA | 通过 | 迷你播放器文本/按钮/进度已枚举 |
| 托盘快捷键文本 | 通过（代码路径） | 绑定变化时更新 |
| 手柄开关 | 通过（代码路径） | 关闭即停表 |
| 中文/日文 IME | 待实机 | 原生 EDIT 未改动 |
| 混合 DPI 多显示器 | 待实机 | 缩放功能已移除，仅系统 DPI |
| 睡眠唤醒/设备切换/长时间播放 | 待实机 | 未在本轮执行 |
| Explorer 真实 OLE 拖放 | 待实机 | 注册成功，未手工拖拽 |

- 已知限制：模糊封面首次生成仍发生在 UI 绘制线程（一次，毫秒级）；
  未做后台服务化。若未来支持超大 8K 客户区，再考虑移出 UI 线程。

## 28. 后续修正：窗口恢复 / 切歌卡顿 / 乱序队列 / mp3list 恢复 / overlay 帧率（2026-11-11）

### 28.1 前台恢复与最小化

- `WM_ACTIVATE`（激活）现在调用 `restore_window`：清除遮挡缓存并整窗重绘。
  之前只重开动画定时器，遮挡轮询最多 20 个 tick（失焦心跳下为秒级）才
  重新发现窗口可见，期间 `invalidate` 被丢弃，表现为“切回前台后 hover
  几秒不响应 + 残影”。失焦时清 hover 与行 tooltip。
- `WM_MOUSEMOVE` 若 `window_visible == false`（缓存过期）先
  `restore_visibility`；鼠标在客户区即证明窗口已露出。
- `WM_SIZE` 从最小化恢复时，若遮挡缓存仍是隐藏状态则强制
  `restore_visibility`，避免整窗失效被丢弃。
- 移除“最小化到托盘”行为与设置项（`minimize_to_tray`、`SetMinimizeToTray`、
  `SettingField::MinimizeToTray`、窗口分类行）。`WM_SYSCOMMAND` 不再拦截
  `SC_MINIMIZE`，最小化留在任务栏。“启动时最小化”改用
  `SW_SHOWMINIMIZED`（此前是 `SW_HIDE`，等同隐藏到托盘）。
- `close_behavior=Tray`（关闭按钮）保持不变。

### 28.2 切歌卡顿（两个 UI 线程阻塞源）

- `Queue::ensure_shuffle_order` 原实现每次 `set_current` 都做
  `shuffle_order.contains(i)`（O(n²)）。8940 首乱序队列每次切歌约 80M 次
  比较，实测 UI 线程阻塞 **345–380 ms**。改为一次 bool 位图校验，正常
  顺序直接返回；修复后同一队列切歌 **1–2 ms**。
  新增 `shuffling_tracks_in_a_large_shuffle_is_linear`（20k 首、50 次
  `set_current` < 1s）与重复/缺失索引修复测试。
- 周期保存 `session.json` 原来在 UI 线程序列化并写盘：6.2 MB / 8938 首
  每次约 **160 ms**（每 5 秒一次）。新增 `SessionWriter` 后台线程：UI 只
  `build_session()` 后投递，JSON 编码与文件 IO 在工作线程；退出时
  `save_now()` 先 `flush()` 队列再同步写最终快照，避免旧快照覆盖新快照。
- 封面 RGBA→BGRA 转换统一到 `Painter::cached_cover`（按图像指针缓存，
  预分配 + 逐像素写入）；模糊背景与自定义背景仍在后台线程解码。

### 28.3 播放队列显示乱序顺序

- `Snapshot.queue_order` 暴露 `Queue::shuffle_order`；`App::rebuild_queue_view`
  在乱序时以播放顺序为基准，再叠加筛选，显示行 → 真实下标映射全链路保持。
- 队列页副标题显示“N 首歌曲 · 乱序播放”；筛选时显示“显示/总数 · 筛选：… · 乱序”。
- 乱序视图下禁用缩略图拖拽排序与右键上移/下移/置顶/置底（视觉顺序 ≠ 队列
  存储顺序），行点击/右键/封面/tooltip 仍先映射真实下标。
- 修复队列行封面请求：`request_visible_covers` 之前用显示行直接取
  `snapshot.queue`，乱序时请求了错误歌曲的缩略图（列表只剩占位符）。
- 切换乱序后立即 `queue_changed()` 并刷新视图/页头，不再依赖动画定时器
  （暂停时没有 tick）。
- 进入队列页自动定位当前播放项：按整行对齐滚动（保留半屏上下文），不会
  留下半截首行压在筛选框外框上。
- 筛选框与列表顶部间距 6 → 12 DIP；滚轮滚动后按光标位置重算 hover，行高亮
  与 tooltip 跟随滚动后的实际行。

### 28.4 mp3list 启动恢复

- 启动自动刷新改为增量：`refresh_startup_playlist` 解析 mp3list（含
  `#SEARCH#` 目录）后只把队列里没有的歌曲追加到末尾，保留上次的队列内容、
  手动编辑与乱序 cycle；不再整表重建。
- `build_session` 持久化当前播放秒数；启动时仅当 `session.current_key`
  与恢复曲目一致才使用，`clamp_start_secs` 忽略 <2s 和末尾 3s 的位置。
  `import_playlist` 重开同一列表时也按 live/saved 位置续播。
- 上次乱序播放时恢复 `shuffle_keys` + cursor（原有逻辑保留，新增测试）。
- 乱序队列播到末尾时始终重洗并进入新一轮（`RepeatMode::Off` 也循环），
  `advance` 实际进入新一轮时才把 `shuffle_order` 切到新顺序，界面不会在
  最后一首播放中途突然重排。

### 28.5 桌面歌词 / 迷你播放器帧率

- `DesktopLyricsSettings.frame_rate`、`MiniPlayerSettings.frame_rate`
  （默认 60，范围 15–120）；`overlay_host` 不再复用主窗口
  `performance.frame_rate`。
- 设置页“桌面歌词/迷你”分类新增“帧率”数值行（可点击输入，Step ±5）。
- overlay 侧原有 `desired_interval()` 直接使用各自配置：暂停/被完全遮挡时
  仍无定时器；KTV 逐字填充按该帧率刷新，未启用逐字时仍为 10 Hz 换行检查。

### 28.6 输入框边框漏画（曲库 → 队列后）

- 根因：`draw_header` 判断“队列筛选框是否脏”的 `queue_toolbar` 矩形用错了
  底边（`queue_import.bottom`，位于 header 内），实际得到 top > bottom 的
  倒置区域，`hits()` 永远为 false。队列页的 `layout.list` 从
  `HEADER_HEIGHT + 6` 开始，正好覆盖筛选框条带；行封面解码
  （`row_cover_rev`）等只失效 `layout.list` 的重绘会用背景色填掉筛选框，
  而 `draw_header` 提前返回，边框/底色不再绘制，直到 hover 等其它失效
  重新覆盖到该条带。
- 与“在别的页停留一会儿（封面加载完成）再切到队列才出现”一致：切页本身
  是整窗重绘，边框会画；随后到达的封面失效把它擦掉。其它含封面加载的页面
  也会偶发同类问题。
- 修复：
  - `layout::queue_toolbar()` 覆盖到 `queue_filter.bottom + 4`，并补单元测试
    `queue_toolbar_covers_the_whole_filter_frame`（非倒置、覆盖整个筛选框、
    与 `layout.list` 相交）。
  - `set_page` 切页时 `reset_visibility()` 并标记 `visibility_changed`，
    防止过期的遮挡缓存把条带排除在整窗失效之外。
  - `sync_edit_boxes` 在页面进入（`page_changed`）或扩展名 EDIT 从隐藏变为
    显示时，用 `invalidate_force` 强制重绘队列筛选框 / 曲库搜索框 /
    文件夹地址栏 / 扩展名输入框的外框，绕过遮挡缓存。

### 28.7 输入框文字下缘被裁切 / 偏上

- 现象：输入框里 g/j/p/q/y 的下缘被控制区底边裁掉；调整后文字又整体偏上或
  盖住下边框。
- 原因：`place_edit` 用 `13.5 × scale + 6` 估算单行 EDIT 高度，但 175% DPI
  且进程 DPI 感知降级时，EDIT 实际绘制用的字体行高大于该值（实测字体
  `-24`、`tmHeight 20`，而控制区只有 17 px），下降部被裁；把控制区缩小后
  又只按控制区居中，而单行 EDIT 的文本是顶对齐的，视觉上偏上；控制区贴到
  外框底部时会盖住父窗口画的下边框。
- 修复：
  - `create_edit_font` 13.5 → 13 DIP，给下降部留余量。
  - 新增 `edit_line_height()`：在 EDIT 自己的 DC 上 `WM_GETFONT` +
    `GetTextMetricsW`，用真实的 `tmHeight + tmExternalLeading` 计算行高，
    不再靠 DIP 比例猜测。
  - `place_edit`：控制区高度 = 行高 + 6，且距外框上下各留 ≥4 px；按
    “文本块中心 = 外框中心”定位（单行 EDIT 顶对齐，所以控制区顶部要上移
    半个行高），既居中又不遮住圆角边框。

### 28.8 曲库工具栏重排

- 搜索框移到页头下方独立一行，右侧同一行放“排序 · 标题”下拉；不再有第二行
  筛选按钮。
- 页头右上角放整库操作：“添加目录 / 重新扫描 ··· 全部入队 / 检查缺失”，
  右对齐，尺寸与其它页面的右上角按钮一致（84×28 DIP，组间 16 DIP）。
- `library_list` 顶部改为 `HEADER_HEIGHT + 44`；`library_controls()` 覆盖
  搜索行 + 页头按钮，脏区判断和 hover 重绘都走这个联合区域；副标题在
  “添加目录”左侧截断避免压到按钮。
- 窄窗口（640 DIP）下四个页头按钮仍右对齐，副标题自动让位；排序按钮固定
  130 DIP，搜索框占剩余宽度。

### 28.9 捕获保护设置入口

- `desktop_lyrics.capture_protection` 一直生效（overlay 调用
  `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`，实测
  `GetWindowDisplayAffinity` 返回 `0x11`，屏幕截图看不到歌词窗口），但设置
  只放在“桌面歌词”分类，用户找不到。
- “窗口”分类新增一行“桌面歌词捕获保护”（`SettingField::DesktopLyricsCaptureProtection`），
  与主窗口“录屏捕获保护”、独立歌词“歌词窗口捕获保护”并列；三行分别对应
  `window.capture_protection` / `window.lyrics_capture_protection` /
  `desktop_lyrics.capture_protection`。
- 运行时切换会经 `desktop_settings_key` 触发 overlay `force_state`，立即重新
  设置 affinity；暂停状态也能生效。
- 注意：`WDA_EXCLUDEFROMCAPTURE` 只对真正的屏幕捕获（BitBlt、Windows
  Graphics Capture、Snipping Tool、OBS 显示器捕获）生效；`PrintWindow`
  类工具按设计可以读到窗口内容，不能用来验收该功能。

### 28.10 智能歌单/查重列表行封面延迟

- 现象：进入“智能歌单”或“重复文件”后，可见行的缩略图一直是音符占位符，
  鼠标移到某一行才出现该行封面；发现页正常。
- 根因：`tick()` 中 `row_cover_rev` 变化时只按页面失效了队列/曲库/搜索、
  发现和文件夹的列表，`Page::Smart` / `Page::Duplicates` 落进 `_ => {}`，
  后台解码完成后没有任何 `InvalidateRect`，只能等 hover 重画该行。
- 修复：缩略图解码完成时统一失效 `self.list_area()`（按页面返回队列/曲库/
  文件夹/搜索/发现/智能/查重的实际列表矩形），不再逐页枚举。列表页内的
  解码结果都会立刻重绘，不会漏页，也不会整窗刷新。
- 注意：`PrintWindow`/截图会触发一次重绘，用截图验收这个 bug 会把问题
  “治好”，只能靠不截图时的真实表现或日志验收。

### 28.11 验证

| 项目 | 状态 | 说明 |
| --- | --- | --- |
| `cargo build` | 通过 | 0 warning |
| `cargo test` | 通过 | **236 passed / 0 failed / 6 ignored** |
| 切歌 UI 阻塞 | 通过 | `SendMessageTimeout(WM_NULL)` 最差 345 ms → 1–2 ms（8938 首乱序） |
| 后台 session 保存 | 通过 | 每 5 秒自动保存，UI tick 无 >20ms 停顿；退出 flush 后同步写 |
| 乱序队列显示 | 通过 | UIA/截图：首行与 `shuffle_keys[0]` 一致，当前曲目高亮正确 |
| 队列行封面 | 通过 | 修复请求映射后缩略图正常显示 |
| 最小化/启动最小化 | 通过 | `IsWindowVisible=true`、`IsIconic=true`，任务栏可恢复 |
| 设置页帧率行 | 通过 | 桌面歌词/迷你分类均出现“帧率”，默认 60 fps |
| 重启恢复 | 通过（单测） | 同列表仅追加新文件；位置与乱序顺序恢复；轮次结束重洗 |
| 输入框边框 | 通过 | `queue_toolbar` 倒置修复；封面失效后筛选框不再被背景擦除 |
| 输入框文字 | 通过 | 真实字体度量定高 + 文本块居中；g/j/p/q/y 下降部完整、四边框可见 |
| 曲库工具栏 | 通过 | 搜索框 + 排序同一行；页头右上角四个操作按钮右对齐 |
| 桌面歌词捕获保护 | 通过 | 窗口分类可切换；affinity=0x11；屏幕截图不含歌词层 |
| 智能歌单/查重行封面 | 通过 | `row_cover_rev` 统一失效 `list_area()`；解码完成即重绘，无需 hover |

- 待实机：中文/日文 IME、混合 DPI 多显示器、睡眠唤醒/设备切换、
  Explorer 真实 OLE 拖放（与 §27.7 相同）。

## 29. 焦点框 / 桌面歌词帧率 / 无目录重扫（2026-11-11）

### 29.1 Tab 焦点框残留

- 现象：Tab 切换焦点后原先的虚线焦点框没清除，新焦点框也不同步出现；侧栏
  会留下几条横向虚线。
- 根因：`paint.rs` 用 `DrawFocusRect` 往**持久双缓冲**里画。它是 XOR 语义，
  而窗口只 blit 脏包围盒：焦点移动后旧框所在区域只重画了一部分，XOR 过的
  像素就留在后台缓冲里；此外 `focus_rect()` 比 `focus_target()` 失效的矩形
  多 inflate 了 1 DIP，边框外侧一圈永远落在失效区外。
- 修复：
  - `focus_rect()` 直接返回 `target_rect()`，绘制矩形与失效矩形完全一致；
  - 新增 `Painter::focus_ring()`，用 `PS_DOT` 虚线笔 + COPY 模式画四边，
    幂等，重复绘制不会翻转，也不依赖脏区是否覆盖整条边框；
  - 焦点变化仍只失效旧/新焦点矩形，不整窗刷新。
- 实测：UIA/`CopyFromScreen` 连续 Tab 6 次，侧栏与玩家栏均无残留虚线。

### 29.2 桌面歌词 60 fps 实际只有 4 fps（KTV 卡顿）

- 两个独立原因叠加：
  1. `render_if_needed()` 把 KTV 行的重画条件写成 `position_dirty`，而该标志
     只在宿主每 250 ms 发一次 `Position` 时置位 → 卡拉 OK 填充实际只有
     **4 Hz**，设置 60 fps 也无效。现在每个渲染 tick 都用插值时钟推进当前行
     （暂停时定时器本就停止），`position_dirty` 字段随之删除。
  2. `SetTimer(16)` 受系统 15.6 ms 时钟粒度影响，实际约 **31 ms 一帧**；
     30 fps（33 ms）会被拉长到 ~47 ms。新增
     `core/platform/timer_resolution.rs`（`timeBeginPeriod(1)` 的 RAII 守卫）：
     桌面歌词/迷你播放器在动画间隔 < 90 ms 时持有 1 ms 分辨率，暂停/隐藏/
     销毁时立即释放，空闲时不影响系统功耗。
- 验证：播放 + 桌面歌词时 `NtQueryTimerResolution` 返回 10000（1 ms），
  关闭程序后回到 156250（15.6 ms）。
- 上限：`SetTimer` 的最小可用间隔是 10 ms，因此 120 fps 实际约 100 fps；
  60 fps 及以下为真实帧率。

### 29.3 桌面歌词“不透明度 100%”仍然透明

- 现象：把桌面歌词不透明度拉到 100%，面板仍能看到后面的桌面。
- 根因：`desktop_lyrics.bg_color` / `bg_color2` 默认带 `185/255` 的 alpha
  （`[14,17,26,185]`、`[32,40,60,185]`），系统取色器只能改 RGB，alpha
  一直保留；渲染时再乘 opacity，所以最高只有 72.5% 不透明。
- 修复：`render.rs` 新增 `opaque()`，纯色/渐变背景先把 alpha 固定为 255，
  透明度只由 opacity 设置控制。模糊封面模式压在封面上的黑色薄纱
  （`[0,0,0,70]`）与细边框保留，它们是有意的着色/描边，不是面板透明度。
  迷你播放器面板的固定 alpha 也从 `242` 改为 `255`，两个 overlay 一致。
- 单测：`full_opacity_background_is_fully_opaque`（100% → alpha 1.0，
  50% → 0.5）。

### 29.4 D2D 渲染目标可切换（统一设置）

- 现象：桌面歌词 / 迷你播放器的 GPU 占用明显高于主窗口滚动歌词，虽然都用
  D2D；主窗口滚动歌词与独立歌词实际是纯 GDI（`ktv_overlay.rs` 已不在构建）。
- 根因：两个 overlay 的 `ID2D1DCRenderTarget` 用
  `D2D1_RENDER_TARGET_TYPE_DEFAULT`。D2D 可能选硬件 target，为一块小 DIB
  每帧做上传/回读，产生可见 GPU 时间；收益为零。
- 修复：
  - 性能分类新增统一开关“D2D 硬件渲染（桌面歌词/迷你）”
    （`performance.d2d_hardware`，`Command::SetD2DHardware`），默认**关**
    （软件光栅，CPU）。
  - 桌面歌词与迷你播放器的 renderer 都按该开关选择
    `D2D1_RENDER_TARGET_TYPE_SOFTWARE` / `HARDWARE`；请求硬件但 D3D 不可用
    时自动回退软件并记日志。Surface 缓存记录实际模式，切换后重建。
  - `desktop_settings_key` / `mini_settings_key` 纳入该字段，点击开关会
    强制推送 state，暂停时也立即重建渲染目标。
- 主窗口滚动歌词/独立歌词不需要该开关：纯 GDI 自绘，无 D2D target。

### 29.5 删除全部曲库目录后“重新扫描”不清空曲库

- 现象：移除所有曲库目录后点“重新扫描”，旧曲库（1 万多首）仍原样显示。
- 根因：`index_roots` 只 upsert 扫到的文件，从不清理不再属于任何根目录的
  `tracks` 行；`start_library_index` 在根目录为空时直接 return，连扫描都不发。
- 修复：
  - 扫描完成（未取消）后新增 `prune_outside_roots()`：删除 key 不在任何
    **已配置**根目录下的行及其 fingerprints。根目录为空时即清空整库；
    配置了但暂时离线的根目录（U 盘/网络盘）下的行保留，避免误删播放统计。
  - `start_library_index` 不再对空根目录提前返回：空列表也是一次真实扫描，
    由 worker 完成清理并回发 `IndexDone`，UI 刷新为 0 首。
- 单测：`rescan_prunes_rows_outside_the_configured_roots`。

### 29.6 验证

| 项目 | 状态 | 说明 |
| --- | --- | --- |
| `cargo build` | 通过 | 0 warning |
| `cargo test` | 通过 | **237 passed / 0 failed / 6 ignored** |
| Tab 焦点框 | 通过 | 连续 6 次 Tab 无残留；旧框即时清除、新框即时出现 |
| 桌面歌词定时器分辨率 | 通过 | 动画中 1 ms，退出后 15.6 ms |
| 100% 不透明度 | 通过（单测） | 面板背景 alpha=1.0；50% 时为 0.5 |
| D2D 渲染切换 | 通过 | 设置项写入 `performance.d2d_hardware`；默认软件，硬件失败回退 |
| 无目录重扫 | 通过（单测 + 便携副本实测） | 根目录为空时 `tracks` 清空；离线根目录的行保留 |


## 30. 播放进度记忆范围 / 重置确认（2026-11-11）

### 30.1 只记“播放到哪一首”，不记曲内进度

- 需求：重启或重开 mp3list 时不恢复具体歌曲 00:17 之类的进度，只恢复队列里
  正在播放的那一项（mp3list 的播放位置）。
- 修改：
  - `build_session()` 的 `position_secs` 恒写 `0.0`（字段保留，兼容旧 session
    文件；旧文件里的非零值在启动时也被忽略）。
  - 启动恢复与 `import_playlist()` 一律 `load_index(..., 0.0, ..)` /
    `restore_paused(..., 0.0)`；删除 `startup_resume_secs()` 和导入时按
    `live_position`/`session.position_secs` 计算 `resume_secs` 的逻辑。
  - 队列、`current_key`、shuffle 顺序与游标照旧恢复，所以会回到上次播放的
    那一首，并从 0:00 开始。
- 测试：`session_restores_the_track_but_not_the_position`（旧 session 写
  12.0 也按 0:00 恢复）、`start_offsets_are_clamped_for_explicit_seeks`。
- 验证：重启后 session 写入 `position_secs: 0.0`，`current_key` 仍是原曲。

### 30.2 重置全部设置的确认弹窗（已有，确认无需修改）

- 设置 → 系统 → “恢复默认设置…” 产生 `SettingsAction::Reset`，
  `perform_settings_action()` 用原生
  `MessageBoxW(MB_YESNO | MB_ICONWARNING)` 提示“恢复所有设置为默认值？
  这会移除自定义颜色、歌词/overlay 样式和目录设置。”
- 只有点“是”才执行 `AppCore::reset_settings()`、重新应用设置并 toast
  “设置已恢复默认”；点“否”直接返回，不改任何配置。
