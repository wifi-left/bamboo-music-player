# 原生 UI（M1）经验与踩坑

> 2026-11-09，根目录 `bamboo-music-player`（Win32 + GDI + Direct2D）。
> 环境：Windows 11，175% DPI，Intel 核显，debug 构建。

## 1. 架构结论

> 2026-11-09 更新：主窗口的“正在播放”已改为歌曲卡片 + **可滚动歌词子窗体**
> （`src/lyrics_view.rs`，纯 GDI 双缓冲，自己处理滚动/悬停/点击/滚动条/KTV）。
> 下文的“KTV 分层子窗口”方案已从主窗口停用（`ktv_overlay.rs` 保留作 D2D 参考），
> 但仍是桌面歌词/迷你播放器等覆盖窗口的实现约束。

- **主窗口全部 GDI 自绘**：背景、侧栏、虚拟列表、播放条、进度/时间、
  歌曲卡片、歌词面板外壳都画到 `BeginPaint` 的 DC；`WM_PAINT` 已由系统裁剪到
  更新区，应用再按脏矩形跳过不相交元素。字体/画刷缓存。
- **KTV/滚动歌词使用独立分层子窗口**（MFC 式控件；主窗口已改用 GDI 歌词子窗体，
  以下规则适用于桌面歌词/迷你播放器等覆盖表面）：
  - `WS_CHILD` + `WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE`；
  - 内部 `ID2D1DCRenderTarget`（**软件渲染**）画到持久 DIB；
  - `UpdateLayeredWindowIndirect` 的 `prcDirty` 只发布变化条带；
  - `WS_EX_TRANSPARENT` 鼠标穿透，键盘焦点留在主窗口。
- 子窗口无自己的计时器；父窗口在 `WM_TIMER`/输入/尺寸变化时调用 `update()`，
  非 Now Playing 页隐藏子窗口。
- 下一行歌词提示仍由父窗口 GDI 绘制，KTV 子窗口只覆盖当前行条带。
- `app.manifest` 声明 Windows 8/8.1/10+ `supportedOS` 与 PerMonitorV2；
  这是分层子窗口可用的前提。
- 输入框方向：透明背景、无边框 `EDIT` + 自绘背景，不自绘文本编辑器。

## 2. 关键踩坑

### 2.1 不要在同一窗口表面混画 D2D DC target 与 GDI

- `ID2D1DCRenderTarget` 先渲染到**内部位图**，再由 GDI `BitBlt` 到绑定区域；
  **绑定区域会被整块覆盖**。
- 绑定完整客户区只画一条 KTV，会把整窗 GDI 内容刷黑/覆盖。
- 只绑定脏子矩形时，子矩形会成为 D2D 坐标原点，且 DWM 可能不更新实际画面
  （`PrintWindow` 能看到、桌面看不到）；坐标/DPI 映射也更易出错。
- 结论：**D2D 内容必须隔离到独立窗口/表面**，不要和 GDI 争同一张窗口 DC。

### 2.2 `WS_EX_LAYERED` 子窗口的创建顺序

- `CreateWindowExW` 直接带 `WS_EX_LAYERED + WS_CHILD` 会失败：
  `ERROR_INVALID_HANDLE (0x80070006)`。
- 正确做法：先用普通 `WS_CHILD` 创建，再
  `SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ... | WS_EX_LAYERED)`，
  然后 `SetWindowPos(..., SWP_FRAMECHANGED)`。
- 没有 manifest 时 `GWL_EXSTYLE` 设置会**被忽略**（读回 `0x0`）；
  `app.manifest` 声明 Win8+ `supportedOS` 后生效。
- `WS_EX_LAYERED` 与 `WS_EX_TRANSPARENT` 同时在 `CreateWindowExW` 里传会破坏
  `UpdateLayeredWindowIndirect`（Qt 已知问题）；必须创建后再加。

### 2.3 小覆盖层不要用硬件 D2D DC target（内存大坑）

- `D2D1_RENDER_TARGET_TYPE_DEFAULT` 对一块很小的 DIB 也会初始化 D3D/显卡驱动：
  进程私有提交 **+约 30MB**（36MB vs 6MB），且与窗口尺寸无关。
- 小面积覆盖层使用 `D2D1_RENDER_TARGET_TYPE_SOFTWARE`；CPU 成本极低
  （`--bench` 平均绘制约 0.4ms/帧）。
- 验证方法：`PROCESS_MEMORY_COUNTERS` 的 `PagefileUsage` 是私有提交，
  `WorkingSetSize` 含共享 DLL；两者要分开看。
- 对照：裸 Win32 窗口（含 GDI 绘制/字体/D2D+DWrite 工厂）私有约 2MB；
  硬件 D2D target 版本约 36MB；软件 target 版本约 6MB。

### 2.4 局部刷新要用桌面截屏验证

- `PrintWindow`/窗口表面截屏可能显示已更新，但 DWM 实际画面仍是旧帧；
  必须用桌面 `CopyFromScreen` 截取窗口区域，并同时看进程 `Responding`。
- `ShowWindow` 会触发 `WM_SIZE`；如果 `WM_SIZE` 里创建 D2D/子窗口资源，
  首次初始化可能发生在 `ShowWindow` 内，排查内存时要按阶段打点
  （窗口前/后、首帧前后、KTV 更新前后）。

### 2.5 动画脏区与发布

- KTV：用 `IDWriteTextLayout` 的 metrics 得到文本 span；每 tick 只失效
  `old_fill → new_fill` 的条带；行切换/seek/页面切换才整块重绘。
- 进度条：只失效填充边缘（±几 DIP）；时间文本只在显示秒变化时失效。
- `UpdateLayeredWindowIndirect` 的 `prcDirty` 只支持单个矩形；多条带分多次调用；
  失败时回退整窗 `UpdateLayeredWindow`。
- 变化小于约 0.3 DIP 时跳过发布，避免无意义的重绘/合成。
- 暂停/最小化/非 Now Playing：停止父窗口计时器并隐藏 KTV 子窗口。

### 2.6 遮挡/后台时 GPU 反而更高

现象：主窗口被其他窗口遮挡（非最小化）时，播放 + KTV 的 GPU 占用高于前台。

原因（查文档 + 实测）：

1. 被遮挡的窗口会失去 direct/independent flip 或 MPO 路径，回落到 DWM 合成；
   前台可见路径更便宜。GPU 计数器把合成工作算到提交更新的进程上。
2. `UpdateLayeredWindow(Indirect)` 每次都会更新并合成**整块分层表面**，
   `prcDirty` 只是优化提示；覆盖时 DWM 也不会因为“看不见”而完全跳过合成。
3. 分层子窗口不参与顶层窗口的遮挡剔除；每次 ULW 更新都可能触发系统级
   `EVENT_OBJECT_LOCATIONCHANGE` 与其它进程的遮挡重算（Chromium 有对应 bug），
   所以浏览器在后台时更明显。
4. 没有官方“我被遮挡了”的 API；必须自己按 Win32 z-order 判断。

处理：

- 不按焦点判断（失焦不等于看不见）；按 **可见矩形** 判断。
- 每 ~0.5s 枚举 z-order 中位于本窗口之上的顶层窗口，把它们的矩形从客户区
  减去，得到可见矩形列表；全部减完则为完全遮挡。
- 动画脏区与可见矩形求交；父窗口只重绘露出的部分，KTV 子窗口只发布可见条带。
- 完全遮挡时停止所有失效/发布（时钟仍推进）；重新可见时整窗重绘并强制
  KTV 全条带重发，避免露出旧内容。
- 实测：前台/部分遮挡 0.2–0.5% GPU；完全遮挡 **0.00%**；重新可见后恢复。
  部分遮挡的 GPU 不会明显低于前台，因为 ULW 仍按整层合成；收益来自
  “完全遮挡时不再更新”和“不绘制被盖住的像素”。

### 2.7 曲库不要用“动态流加载”的虚拟列表

- 流式列表（先查一页，接近底部再追加）在后台刷新时很脆弱：播放统计/索引事件会
  触发重查，重新排序后 `scroll` 可能落在新数据末尾之外，表现为靠后的行突然消失。
- 曲库改为**整页查询**：每页行数 = `floor((列表区高度 − 分页条) / row_height)`，
  UI 只保存页码，`AppCore` 只保留当前页的行；后台刷新重查同一个 `offset/limit`。
- 这样行号与当前快照天然对齐，不需要“保留已加载行数/clamp scroll”这类补丁；
  搜索、排序、进页回第 1 页，末页越界时回最后一页。
- 队列是内存数据，仍用连续滚动；两种列表共享绘制，但滚动状态分开。

### 2.8 虚拟列表行的 tooltip 用原生 `tooltips_class32`

- `TTF_TRACK | TTF_ABSOLUTE` + `TTM_UPDATETIPTEXT` / `TTM_TRACKPOSITION` /
  `TTM_TRACKACTIVATE`，悬停行变化时更新文本并重启延迟计时。
- 自绘 tooltip 会引入额外脏区和遮挡判断；原生 tooltip 是独立 popup，
  不参与主窗口 `WM_PAINT`，点击/翻页/离开窗口时主动停用即可。
- 450ms 延迟 + 5s 自动隐藏，避免扫过列表时连续弹出。

### 2.9 进度条拖动：捕获 + 本地预览 + 松手才 seek

- 在 `WM_LBUTTONDOWN` 命中真正的 4 DIP 进度条时 `SetCapture`；上下 10 DIP
  的悬停留白不进入拖动。
- 拖动期间只更新本地 `drag_preview`（进度填充、游标、时间文本都读预览值），
  不向音频线程发 seek；`WM_LBUTTONUP` 只发一次 `SeekSecs`。
- 直接在 `WM_MOUSEMOVE` 里连续 seek 会让解码不断重定位，表现为“音频跟随光标”
  和卡顿；主窗口与独立歌词窗口的底部进度条都遵守这条规则。
- 播放统计写库同理：只 `INSERT OR IGNORE` + 更新计数，不能拿播放态的轻量
  `Track` upsert 全行，否则会把索引里的 duration/bitrate 覆盖成 NULL。

## 3. 实测（debug，175% DPI，核显）

- 空闲/暂停：GPU 0.00%
- 播放 + KTV：GPU **0.17–0.27%**
- 私有提交 **6.2MB**，工作集 **23.3MB**
- 平均绘制约 **0.38ms/帧**
- 对照：硬件 D2D target 版本私有 36MB / 工作集 47MB；MFC + VLC 参考约 26MB

## 4. 文件索引

- `src/main.rs`：窗口、消息循环、计时器、输入；`WM_SIZE/WM_DPICHANGED`
  同步 KTV 子窗口几何。
- `src/paint.rs`：GDI 绘制（侧栏/列表/播放条/进度/时间/提示），字体/画刷缓存。
- `src/ktv_overlay.rs`：分层子窗口 + 软件 D2D DC target + `prcDirty` 发布。
- `app.manifest` / `build.rs`：supportedOS + PerMonitorV2 + 图标资源嵌入。
- `docs/WIN32_UI_PLAN.md` §3.1 / §8.1：指标与渲染路径。

## 5. 后续建议

- M0 业务核心迁入 `src/core/`；UI 只读 `Snapshot`、只发 `Command`。
- 曲库/队列列表可评估独立子控件（隔离滚动/悬停），但预期内存收益有限
  （窗口表面是同一顶层窗口的重定向表面）。
- 搜索/设置输入使用透明无边框 `EDIT`。

### 2.10 歌词固定字号与横向跟随

- 不为放下一行而缩小字号：固定字号是后续“自定义歌词字号”的前提；
  超宽由横向跟随/省略号解决。
- 当前行的横向跟随在**已唱边缘到达行宽 7/8** 时启动，保留右侧 1/8 留白；
  文字与 KTV 填充共用偏移和裁剪。非当前行不滚动，翻译行保持居中。
- 其他行仍用省略号；暂停/短行不产生失效，滚动只随 KTV 进度重绘当前行。

### 2.11 窄屏时把播放条功能放进齿轮面板

- 播放条 <900 DIP 隐藏桌面歌词/迷你播放器，<820 DIP 隐藏音量滑块；
  齿轮面板（`player_panel.rs`）补充音量滑块与这些入口，避免功能不可达。
- 桌面歌词/迷你播放器两行现在是真实开关（开/关底色与播放条同步）；
  宽屏不重复显示。面板高度随窄屏增加，布局由 `layout.rs` 同步。

### 2.12 暂停后 overlay 图标不刷新：状态变化必须立刻推送

- 现象：点迷你播放器的暂停按钮，音乐停了但按钮仍是“暂停”图标；
  拖动一下窗口又恢复正常。暂停状态下点播放则能刷新。
- 根因：主窗口的 16ms 动画定时器在暂停时停掉；overlay host 只在
  “全量状态 500ms / 位置心跳 250ms”时推送。如果点击恰好落在两次心跳之间，
  暂停后不会再有定时器驱动下一次心跳，新状态永远发不出去。恢复播放会重新
  启动定时器，所以看起来只有暂停坏。
- 修复：host 记录上一次 `playing`，播放状态变化时**立即**发全量状态；
  处理 overlay 动作后再 `force_state()` 一次（音量/静音/切歌等暂停状态下
  发生的变化也立即回显）。`state_due()` 是纯函数并有单测。
- 约定：任何“暂停后仍可能变化”的 overlay 状态（播放态、音量、静音、
  当前曲目、歌词、封面）都必须能主动唤醒 UI 并强制推送，不能只依赖周期心跳。

### 2.13 封面绘制与 DPI

- 封面 RGBA 在 UI 线程只做一次 BGRA 转换，按图片地址缓存；用
  `StretchDIBits` + HALFTONE 居中裁方形，失败回退占位音符。
- 当前曲目封面与 64px 行缩略图是两套后台服务：切换曲目会取消旧的当前封面
  请求，但不会取消列表缩略图；列表只请求可见行，`None` 结果也缓存。
- 桌面歌词线程必须和迷你播放器一样显式
  `SetThreadDpiAwarenessContext(PER_MONITOR_AWARE_V2)`；否则 175% 下
  `GetDpiForWindow` 报 96，窗口按半尺寸创建，而 D2D 表面按系统 DPI 渲染，
  表现为窗口/文字比例错乱。

### 2.14 透明分层窗口不能当遮挡物

- 遮挡计算按 z-order 枚举上方窗口并从客户区减去。分层窗口
  （`WS_EX_LAYERED`）可以有任意 alpha，tooltip/overlay 也可能点击穿透
  （`WS_EX_TRANSPARENT`）；把它们的矩形当作完全不透明会导致主窗口在透明
  区域下方永远不重绘，表现为“播放/切歌后有一块旧画面”。
- 修复：`visible_rects` 跳过这两类窗口。被透明窗口盖住的动画脏区会被多画
  一次，但脏区本来就只包含进度边缘/当前歌词行等小矩形，实测开销可忽略；
  因此没有增加“忽略透明遮挡”的设置项。
- 只有在叠加超大分层窗口且 CPU 明显上升时，才考虑提供“把分层窗口视为遮挡”
  的省电选项（默认关闭，优先正确性）。

### 2.15 设置页分类与队列筛选的映射

- 设置页用分类按钮（曲库/播放/窗口/系统）而不是把所有分组堆进一条长列表：
  曲库页的原生扩展名 EDIT、文件夹列表与通用行布局互不干扰；切换分类时
  只显示当前分类的原生子控件。
- 通用设置行由 `layout::setting_row/control/segments/step_rects` 统一计算，
  绘制与命中测试使用同一份矩形；新增设置只需加一个 `SettingField` 与命令。
- 暂停时没有动画定时器，设置变更不会等到下一 tick 才保存；离散开关后调用
  `AppCore::flush_settings()` 立即落盘，连续拖动（音量/进度）仍走节流保存。
- 队列筛选不能直接拿显示行当队列下标：`queue_view` 保存“显示行 → 真实
  队列下标”的映射，行点击/右键/封面/tooltip/当前行高亮都必须先映射。
  任何队列变更通过 `Snapshot.queue_rev` 触发重建；筛选变化重置滚动。
- 右键“上移/下移/置顶/置底”只移动用户看到的 items 顺序；`Queue::move_item`
  同时重映射 current/history/shuffle_order/pending_cycle。筛选激活时排序项
  置灰，避免“移动一行却跨过隐藏行”的歧义。
- 侧栏“我的歌单”已移除；`Session.playlists` 数据保留但不再展示。
- 单实例守卫必须在创建窗口之前执行；第二实例只转发命令行后退出。便携模式
  不写注册表和系统跳转列表，避免测试/绿色版污染已安装环境。


### 2.16 随机顺序缓存与封面 WIC 解码

- 随机顺序用 track key 持久化（不是下标），并额外保存 cursor/cursor_key：
  mp3list 重开时旧曲目保持相对顺序，新扫描到的文件追加到末尾；当前曲目
  优先按 key 恢复，文件消失时按 cursor 兜底。
- 每个 mp3list 的随机状态单独缓存（最多 8 个），切换歌单再回来不会丢进度。
- 大封面不要先完整解码再缩略：Windows WIC 的 clipper + scaler 在解码链路里
  直接得到 640px 方形 RGBA；实测 4000×3000 JPEG debug 34ms vs image crate
  3.35s。WIC 失败（少见格式）再回退 image crate。

### 2.17 运行时调色板不能只换主窗口

- `paint.rs` 的颜色常量已改为 `theme::Palette`（暗/亮 + 强调色派生）。
  颜色来源只有一处：`App::palette()`；`Painter::draw_gdi` 每帧刷新，
  `WM_DRAWITEM` 前调用 `set_palette`。
- 原生控件的画刷不会自动跟随：`EDIT`/排序 `LISTBOX` 的背景刷创建于启动，
  主题变化后必须 `DeleteObject` 旧刷并重建（`sync_native_brushes`），
  同时 `InvalidateRect` 三个原生窗口。
- 独立歌词窗口是**有意**的沉浸式深色表面，不跟随 `ThemeMode`；但强调色、
  当前行/普通行/翻译行颜色仍读取 `LargeLyricsSettings`，避免四套硬编码。
- 内嵌歌词面板在浅色主题下只对“仍是默认值”的文字颜色做深色替换；
  用户改过的颜色必须原样使用，不能强制反色。
- 主题切换允许一次整窗重绘；之后仍只失效变化控件。`Palette` 是 Copy
  小结构，比较/构造成本可忽略，不要为它引入全局静态。

### 2.18 设置页分类与通用行

- 分类超过 4 个时用两行 tab（宽 82 DIP + 6 间隙），`SettingsLayout.tabs`
  与内容起点 `top` 一起下移；不要在窄窗口用单行 7 个 tab。
- 通用行可能超过一屏（播放 19 行、迷你 12 行）：`App.settings_scroll` +
  `layout::settings().generic` 视口，绘制时 `IntersectClipRect`，命中测试与
  `target_rect` 都减去同一偏移。切换分类归零，窗口缩放/DPI 变化后 clamp。
- 控件列右对齐、最大 320 DIP，避免步进器的 `−`/`+` 被拉到窗口两端；
  窄窗口自动退化为剩余宽度，标签列固定 260 DIP。
- 滚轮命中用 `generic` 视口，不要复用 `list_area()`：设置页的列表矩形
  不覆盖右侧控件，且语义不同。

### 2.19 歌词字幕设置（words_only / 对齐 / 字号）

- `karaoke_words_only` 的语义是“有词级时间才逐字填充”，不是“关闭 KTV”：
  普通计时 LRC 仍显示当前行（整行高亮/当前色），不得回退成报错或空白。
- 字号保留旧观感：内嵌当前行 = `lyrics.font_size - 2`，普通行再 -1；
  独立窗口 = `large.font_size - 3`。行高由字号推导，行距直接取设置。
- 右对齐关闭横向跟随；左/中对齐仍按“已唱边缘到 7/8 才左移”。
  “减少动态效果”同样关闭跟随，但进度/KTV 颜色继续更新。
- 外观参数整体比较（`Style: PartialEq`），变化时清字体缓存 + 整块重绘一次；
  播放中的 KTV 仍只失效当前行。

### 2.20 暂停时修改迷你播放器设置

- 迷你/桌面 overlay 的 host 只在全量状态 500ms 或位置心跳 250ms 时推送；
  暂停后主窗口计时器停止，设置变化可能永远发不出去。
- 主窗口保存 `MiniSettingsKey`（显示/行数/字号/宽度/置顶等），每次
  `sync_overlays` 比较；变化时 `host.force_state()` 再 `sync`。
- `WM_LBUTTONUP` 也要调用 `sync_overlays`：设置点击发生在暂停态，不能等
  下一次 tick。

### 2.21 均衡器“自定义”预设

- `SetEqBand` 把预设切到 `Custom`；选择 `Custom` 时**不要**用
  `preset.bands()` 覆盖推子（那是全零）。先判断 `preset != Custom` 再写入。
- 预设选择器用循环步进（8 项），避免最小窗口下 8 段按钮挤成不可点的小块。

### 2.22 桌面歌词设置页与 bench 的 overlay 门禁

- 桌面歌词设置分类有 27 行，必须走通用行滚动；不要在分类页里再塞固定
  子控件。字号步进要同时缩放 next/prev（保持 0.76/0.70 比例），否则
  当前行变大、上下行不变，视觉层级会崩。
- `sync_overlays` 第一行是 `if !state.persist { return; }`：`--bench` /
  `--self-test` 不创建桌面歌词/迷你窗口，用来避免污染用户配置和桌面。
  因此验证 overlay 开关必须普通运行；bench 只能验证设置页绘制。
- 设置页点击后 `WM_LBUTTONUP` 已经调用 `sync_overlays`，暂停状态也能立即
  启停 overlay；不要只依赖 16ms 动画定时器。
- 便携验证：在 exe 同目录放 `portable.txt` 会使用 exe 目录下的
  settings/session；测试完应删除该文件，避免后续 `cargo run` 意外进入
  便携模式、读到测试队列。

### 2.23 原生取色器与设置滚动条

- 颜色行不要自绘取色面板：用 `ChooseColorW`（comdlg32）即可获得系统色盘、
  最近色和自定义色槽。`CC_RGBINIT` 传入当前颜色，`lpCustColors` 指向
  16 个 `COLORREF`；把数组保存在 `State` 里，跨多次取色保留。
- 取色器是模态对话框，会重入消息循环：点击时只记录 `App.color_request`，
  `WM_LBUTTONUP` 在 `State` 借用结束后再打开；否则会在 `&mut State` 存活期间
  重入 WndProc。
- RGBA 颜色（桌面歌词描边/阴影）只替换 RGB，alpha 保留原值；不要用
  `[r,g,b,255]` 覆盖，否则半透明描边会变成实心。
- 颜色改变后必须立即 `sync_lyrics`（内嵌子窗体按 `Style` 比较刷新）和
  `sync_overlays`（桌面 host 靠 `DesktopSettingsKey` 变化 `force_state`），
  暂停状态下没有动画定时器，不能等下一次 tick。
- 设置页滚动条与列表滚动条一样需要：内容超出才显示、滑块最小 24 DIP、
  点击轨道/滑块即拖动、`SetCapture`、悬停/按下变色。
- **控件行必须使用 `SettingsLayout.generic`（右侧预留滚动条宽度）**：
  用完整宽度算行/控件矩形会让最右侧的“+”按钮压在滚动条下面。

### 2.24 设置项数值输入用原生 EDIT

- 字号之类需要精确输入的 stepping 行：点击 value 区域显示一个居中的
  无边框 EDIT（第三个原生输入框），而不是弹对话框。Enter 提交、Esc 取消、
  失焦提交；点击时 `EM_SETSEL(0,-1)` 全选，方便直接输入。
- 子类化 EDIT 捕获 `VK_RETURN/VK_ESCAPE` 后 `PostMessage` 给父窗口提交/取消；
  不在子类里直接操作 `State`。
- 显示/定位放在借用结束后的 `sync_number_edit`：行滚动或切页时自动收起；
  EDIT 只覆盖 value 矩形，行高变化（密度/DPI）时跟着 `place_edit` 走。
- 数值范围只在 `Command` 处理器里 clamp 一次；渲染层若还有 clamp，必须同步
  放宽，否则用户输入 120 但画面仍按 48 显示。
- 注意 `SetLyricsFontSize` 与 `SetLargeLyricsFontSize` 已经是独立设置，
  不要再互相覆盖。

### 2.25 设置滚动条的两个顺序坑

- **滚轮必须同时失效行区和滚动条**：`settings_scroll` 变化时只失效
  `generic`，滑块要等鼠标移到滚动条上（hover 失效）才更新。
- **拖动必须在“generic 守卫”之前启动**：滚动条在 `generic` 之外，
  点击处理开头若先 `if !generic.contains { return }`，`SetCapture` 永远
  不会执行，表现为“能滚轮不能拖”。
- 行/控件矩形统一用 `generic`（右侧预留 12 DIP），否则轨道压在“+”上。

### 2.26 桌面歌词完整外观与数值输入

- 桌面歌词剩余项：背景图片（选完自动切 Image 模式）、清除图片、
  背景色/渐变第二色、字体（`ChooseFontW`）、描边宽度、阴影偏移、
  翻译/页脚各自的描边与阴影、全屏隐藏。
- `ChooseFontW` 与 `ChooseColorW` 同属 `Win32_UI_Controls_Dialogs`；
  字体对话框填好 `lfFaceName` 当前值 + `CF_INITTOLOGFONTSTRUCT`，
  返回后从 `lfFaceName` 读 UTF-16 面名。
- 桌面设置 key 要包含字体族、背景图片路径、描边宽度等**非 Copy** 字段，
  结构体从 `Copy` 改为 `Clone + PartialEq`，比较用 `as_ref()` 避免移动。
- 数值输入现在覆盖所有 step 行：字号、行距、透明度（按百分比）、宽高、
  圆角、内边距、描边宽度、阴影偏移、交叉淡化、预增益、EQ 增益、迷你宽度。
  提交统一映射到已有的绝对 `Command`，百分比在映射时除以 100。

### 2.27 文件夹浏览页复用列表基础设施

- 新增页面不要另造一套滚动：`Page::Folders` 直接复用 `row_count/row_info/
  row_rect/visible_rows/max_scroll/scrollbar` 与 `list_area()`；
  `row_info` 返回名称 + 类型，绘制里再加图标分支即可。
- 目录读取是异步的，完成后必须 `browse_rev++`；UI 在 tick 里看到变化后
  重置 hover/scroll 并只失效 header、列表、滚动条。路径不匹配的迟到结果丢弃。
- 滚动条抽成 `App::scrollbar_rect()`（Queue/Folders），绘制、命中、拖动都用它，
  不要把 `layout.scrollbar` 写死在队列页。
- 单击目录进入、单击文件播放（主窗口没有 `CS_DBLCLKS`，不要依赖双击）。
  Backspace 返回上级；`.mp3list` 走 `import_playlist`，其余走 `add_paths`。
- 命令行参数解析要跳过开关的“值”：`--page folders`、`--folder <dir>`、
  `--library <dir>` 的值不是要入队的文件；否则 `--folder` 会同时把整个目录
  加进队列并自动播放。
- 目录行不发封面解码请求（`row_cover` 直接返回 None），图标用
  Segoe MDL2（文件夹/音符/列表）即可。

### 2.28 文件夹工具栏、地址栏与历史栈

- **画在 header 函数里 ≠ 位于 header 矩形内**：文件夹工具栏在 `layout.header`
  （0–64 DIP）下方，但绘制代码在 `draw_header`。该函数用
  `hits(layout.header, dirty)` 提前返回，于是 hover 只失效按钮矩形时，
  背景被重绘、按钮被跳过——表现为“鼠标移上去按钮就消失”。
  修复是用 `layout::folder_toolbar()`（两行工具栏的包围盒）一起参与脏区判断。
  以后在 header 函数里画任何 header 之外的控件，都要把它纳入脏区判断。
- **地址栏用常驻原生 EDIT**：`WS_CHILD | ES_AUTOHSCROLL`、无边框、透明背景，
  父窗口自绘边框（聚焦时强调色）。不要自绘文本编辑。
- Enter/Esc 通过 `SetWindowLongPtrW(GWLP_WNDPROC)` 子类化 EDIT 转发
  `WM_APP_FOLDER_GO / CANCEL`，不要在自己的子类过程里直接导航：
  模态/重入风险和状态借用都要留给父窗口。
- 未聚焦时才把当前路径写回 EDIT；聚焦时不要覆写用户输入。点击行/后退/前进/
  上一级后在 `sync_edit_boxes` 里同步文本。
- **历史栈放核心**（`FolderBrowser.back/forward`），UI 只读
  `can_go_back/forward`。刷新同一路径不增加历史；新导航清空 forward。
  “上一级”是父目录导航，与后退历史是两件事，两者都不要互相替代。
- 文件夹页右键菜单复用 `RowMenuData/RowMenuOutcome`；`list_row_at` 必须把
  `Page::Folders` 纳入，否则右键永远命中不到行。
  播放列表条目不提供“加入队列”（导入会替换队列），目录才提供“加入曲库”。

### 2.29 曲库整库操作与页头工具栏

- **“全部入队”不能用当前页行**：曲库是整页查询，`library.rows` 只有一页。
  整库操作必须在 DB 线程跑一次不带 `LIMIT/OFFSET` 的查询（`QueryAll`），
  用独立请求 id 丢弃过期结果；不要为了“全部”把行数上限调大后在前台轮询。
- **工具栏行同样要进脏区判断**：曲库工具栏（`layout.library_toolbar`）位于
  `layout.header` 下方、由 `draw_header` 绘制。`draw_header` 的脏区条件
  需同时测试 `folder_toolbar`、`library_toolbar`，否则 hover 会擦掉按钮
  （同 §2.28）。
- 扫描状态只影响文案、不新增控件：`library_index_active` 时“重新扫描”
  变“取消索引”，点击调 `cancel_index`。少一个按钮就少一处窄屏挤压。
- 缺失检查与移除索引是两种语义：检查只更新 `missing` 标记（行被查询过滤），
  移除才删索引行。移除必须二次确认并明确“不删除文件”；播放中的曲目不受影响。
- 整库查询结果通过 `DbEvent::AllDone` 回到 UI 线程再 `Queue::add_many`，
  不在 UI 线程打开 DB，也不在查询线程改队列。

### 2.30 设置文件导入/导出

- 导入必须严格解析：启动用的 `load_settings` 会在坏 JSON 时静默回落默认值，
  用于“导入”会丢掉用户全部配置。用 `load_settings_strict` 返回错误并用
  `MessageBoxW(MB_ICONERROR)` 告知。
- 整体替换设置后要重跑一遍外壳初始化：字体缓存、有效 DPI/布局、本地位图、
  原生 EDIT 字体、歌词子窗体、播放设置弹窗、overlay、系统集成、托盘。
  统一放在 `apply_reloaded_settings()`，不要在导入函数里直接重绘。
- 设置里的目录/扩展名变化要 `sync_library_watch()` + `start_library_index()`；
  音量/静音/DSP 也要立即应用，不能等下一次 autosave。

### 2.31 界面缩放 = 有效 DPI

- 不要把 ui_scale 乘进每个 DIP 数值。把它乘进**有效 DPI**：
  `dpi = GetDpiForWindow * ui_scale`，布局仍全部用 DIP。字体、列表、原生
  EDIT/LISTBOX、鼠标坐标换算会自动一致。
- 启动、`WM_SIZE`、`WM_DPICHANGED`、设置变化后的重载四处都要用同一公式；
  漏一处会出现“布局放大了但鼠标命中偏移”或“字体没变”。
- 放大后 640×460 逻辑最小尺寸可能大于当前客户区；窗口在 `WM_GETMINMAXINFO`
  之外也要主动 `grow_to_min_client()`（恢复位置/首次显示/运行时缩放）。
- 缩放变化需要 `painter.dpi_changed()` + `painter.resize(有效 DPI)`；
  只 `InvalidateRect` 会让旧字体/旧位图尺寸残留。

### 2.32 动画帧率上限

- 主窗口动画定时器间隔取 `performance.frame_millis()`（`frame_rate` 夹在
  15–120，最小 8ms）。修改设置后要立即重启定时器，不能等下次
  `sync_timer`（暂停时不会自然发生）。
- 拖进度条/歌词 KTV 仍按脏区失效；帧率上限只改变定时器频率，不改变
  “无变化不重绘”的规则。

### 2.33 迷你播放器配色与文件夹缩略图

- 迷你配色的完整 `MiniColors` 由 3 个可配置颜色派生（卡片/悬停/分割线混白，
  弱化文字向面板混），不要暴露十几个颜色项。颜色必须进入
  `MiniSettingsKey`，否则暂停时改色不会推送（主窗口没有动画定时器）。
- 文件夹页音频缩略图复用 `RowCoverService`：
  - 目录读取完成（`browse_rev` 变化）后要主动 `request_visible_covers()`，
    仅在 `set_metrics`/滚动时请求会漏掉首次进入；
  - `row_cover_rev` 变化要失效 `folder_list`（不是 `layout.list`）；
  - 目录/播放列表不请求封面，保持 MDL2 图标。
- 测试封面时注意 `assets/default_cd.png` 本身就是一个音符图形，看起来会和
  行内图标一样；用彩色测试图确认缩略图真的画出来了。
