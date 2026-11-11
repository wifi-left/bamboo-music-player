//! All layout math in logical pixels (DIPs). The painter converts to device
//! pixels only when talking to Win32 (`InvalidateRect`) or Direct2D.
//!
//! Widget rectangles are stable for the lifetime of a window size, so a state
//! change can invalidate exactly one rectangle instead of the whole window.

pub const SIDEBAR_WIDTH: f32 = 216.0;
pub const PLAYER_HEIGHT: f32 = 92.0;
pub const HEADER_HEIGHT: f32 = 64.0;
pub const ROW_HEIGHT: f32 = 40.0;
/// Height of the library pager bar (prev / page label / next / per-page).
pub const PAGER_HEIGHT: f32 = 46.0;
const PAGE_LABEL_WIDTH: f32 = 140.0;
pub const SCROLLBAR_WIDTH: f32 = 10.0;
pub const NAV_ITEMS: usize = 11;
/// Height of the audio-visualizer strip on the Now Playing page.
pub const VISUALIZER_HEIGHT: f32 = 64.0;
/// Order: 选择歌词 / 独立窗口 / 大歌词 / 编辑 / 刷新.
pub const LYRIC_BUTTON_COUNT: usize = 5;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Rect {
    pub const fn new(left: f32, top: f32, right: f32, bottom: f32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub fn width(self) -> f32 {
        (self.right - self.left).max(0.0)
    }

    pub fn height(self) -> f32 {
        (self.bottom - self.top).max(0.0)
    }

    pub fn contains(self, x: f32, y: f32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }

    pub fn intersects(self, other: Rect) -> bool {
        self.left < other.right
            && other.left < self.right
            && self.top < other.bottom
            && other.top < self.bottom
    }

    pub fn inset(self, dx: f32, dy: f32) -> Self {
        Self::new(
            self.left + dx,
            self.top + dy,
            self.right - dx,
            self.bottom - dy,
        )
    }

    pub fn inflate(self, amount: f32) -> Self {
        self.inset(-amount, -amount)
    }
}

#[derive(Clone, Copy)]
pub struct Layout {
    pub size: (f32, f32),
    pub sidebar: Rect,
    pub logo: Rect,
    pub nav: [Rect; NAV_ITEMS],
    /// Clipped area the navigation list scrolls inside; the version footer
    /// below it is never covered.
    pub nav_viewport: Rect,
    /// Thin scrollbar, zero-sized while every item fits.
    pub nav_scrollbar: Rect,
    /// Total height of all navigation rows at the fixed 38 DIP step.
    pub nav_content_height: f32,
    pub content: Rect,
    pub header: Rect,
    /// Library search box (native EDIT child) and its header buttons.
    pub search: Rect,
    pub sort: Rect,
    pub add_folder: Rect,
    pub rescan: Rect,
    pub list: Rect,
    /// Queue page: filter toolbar below the header, rows below it.
    pub queue_list: Rect,
    pub scrollbar: Rect,
    /// Library page: rows above the footer, pager controls below.
    pub library_list: Rect,
    /// Library page toolbar between header and list (whole-library actions).
    pub library_toolbar: Rect,
    pub library_queue_all: Rect,
    pub library_check_missing: Rect,
    pub library_pager: Rect,
    pub library_prev: Rect,
    pub library_next: Rect,
    pub library_page_label: Rect,
    pub library_page_size: Rect,
    /// Queue page header shortcuts.
    pub queue_import: Rect,
    pub queue_export: Rect,
    pub queue_clear: Rect,
    /// Queue filter box (native EDIT), left of the shortcut buttons.
    pub queue_filter: Rect,
    /// Online search: one toolbar row under the header (input + 3 buttons),
    /// rows below it and the list scrollbar.
    pub online_search: Rect,
    pub online_kind: Rect,
    pub online_queue_all: Rect,
    pub online_random: Rect,
    pub online_list: Rect,
    pub online_scrollbar: Rect,
    /// Album/artist detail: toggle between list rows and a cover grid.
    pub online_wall: Rect,
    /// Download manager: toolbar buttons and the task list.
    pub download_clean_part: Rect,
    pub download_clear_finished: Rect,
    pub download_list: Rect,
    pub download_scrollbar: Rect,
    /// Discover: refresh button and the random-song list.
    pub discover_refresh: Rect,
    pub discover_list: Rect,
    pub discover_scrollbar: Rect,
    /// Folder browser: toolbar buttons, address bar, virtual list and its
    /// scrollbar.
    pub folder_back: Rect,
    pub folder_forward: Rect,
    pub folder_up: Rect,
    pub folder_refresh: Rect,
    pub folder_address: Rect,
    pub folder_play_all: Rect,
    pub folder_queue_all: Rect,
    pub folder_list: Rect,
    pub folder_scrollbar: Rect,
    pub player: Rect,
    pub cover: Rect,
    pub track_text: Rect,
    pub shuffle: Rect,
    pub prev: Rect,
    pub play: Rect,
    pub next: Rect,
    pub repeat: Rect,
    /// Volume track; empty when the window is too narrow for a slider.
    pub volume_slider: Rect,
    pub volume_hit: Rect,
    pub mute: Rect,
    /// Opens the playback settings flyout (speed / equalizer / pitch).
    pub player_settings: Rect,
    pub mini_player: Rect,
    pub desktop_lyrics: Rect,
    pub playlist: Rect,
    /// Position of the playback settings flyout (above the gear button).
    pub player_panel: Rect,
    pub progress: Rect,
    pub progress_hit: Rect,
    pub time_left: Rect,
    pub time_right: Rect,
    /// Now Playing: song card (cover + title/meta/badges + actions).
    pub card: Rect,
    pub card_cover: Rect,
    pub card_title: Rect,
    pub card_meta: Rect,
    /// Source/format badges; empty in compact cards and big-lyrics mode.
    pub card_badges: Rect,
    /// `[play/pause, open file]`.
    pub card_buttons: [Rect; 2],
    /// Audio visualizer strip (empty unless enabled on Now Playing).
    pub visualizer: Rect,
    /// Lyrics panel: header (title/source + actions) and the child list area.
    pub lyric_panel: Rect,
    pub lyric_header: Rect,
    pub lyric_buttons: [Rect; LYRIC_BUTTON_COUNT],
    /// Client area of the native lyrics child window.
    pub lyrics: Rect,
}

/// Rectangles for the Settings page. Computed from the same layout so
/// painting and hit-testing cannot drift apart.
pub struct SettingsLayout {
    /// 曲库 / 播放 / 歌词 / 独立 / 桌面 / 迷你 / 窗口 / 外观 / 性能 / 系统 / 网络 / 存储.
    pub tabs: [Rect; 13],
    /// Y position of the first content row (below the tabs).
    pub top: f32,
    pub scan_toggle: Rect,
    pub watch_toggle: Rect,
    pub follow_toggle: Rect,
    pub dedupe_toggle: Rect,
    pub ext_label: Rect,
    pub ext_edit: Rect,
    pub add_folder: Rect,
    pub folders: Rect,
    /// Viewport for the generic setting rows (all tabs except Library).
    pub generic: Rect,
    /// Visible scrollbar for `generic` when a tab has more rows than fit.
    pub scrollbar: Rect,
    /// Label column width, shrunk on narrow windows so the control keeps room.
    pub label_width: f32,
}

pub const SETTINGS_ROW_HEIGHT: f32 = 34.0;
pub const FOLDER_ROW_HEIGHT: f32 = 30.0;
/// Two-line rows on the statistics page (title + artist).
pub const STATS_ROW_HEIGHT: f32 = 38.0;
pub const SORT_ITEM_HEIGHT: f32 = 28.0;
pub const SORT_ITEM_COUNT: usize = 8;

pub fn settings(layout: &Layout) -> SettingsLayout {
    let left = layout.content.left + 32.0;
    let right = layout.content.right - 32.0;
    let tab_gap = 6.0;
    let tab_height = 28.0;
    let row_gap = 4.0;
    let tabs_top = layout.header.bottom + 8.0;
    let mut tabs = [Rect::default(); 13];
    // Flow layout: fill the width from the left and wrap when the next tab no
    // longer fits. Prefer a column count that divides the tab count evenly so
    // the last row is not left half-empty (12 → 6×2 on wide windows, 4×3 /
    // 3×4 when narrow). A prime count such as 13 has no divisor above 1, so
    // fall back to the maximum number of columns that fits.
    let available = (right - left).max(1.0);
    let min_tab = 96.0;
    let max_columns = (((available + tab_gap) / (min_tab + tab_gap)).floor() as usize)
        .clamp(1, tabs.len());
    let columns = (2..=max_columns)
        .rev()
        .find(|count| tabs.len() % count == 0)
        .unwrap_or(max_columns);
    let tab_width =
        ((available - tab_gap * (columns.saturating_sub(1)) as f32) / columns as f32).max(1.0);
    for (index, tab) in tabs.iter_mut().enumerate() {
        let row = index / columns;
        let column = index % columns;
        let tab_left = left + column as f32 * (tab_width + tab_gap);
        let top = tabs_top + row as f32 * (tab_height + row_gap);
        *tab = Rect::new(tab_left, top, tab_left + tab_width, top + tab_height);
    }
    let tab_rows = tabs.len().div_ceil(columns);
    let top = tabs_top + tab_rows as f32 * (tab_height + row_gap) + 8.0;
    let generic_bottom = layout.player.top - 16.0;
    // Wide windows keep the historical 260 DIP label column; below that the
    // column shrinks so the controls never fall under ~180 DIP.
    let label_width = (available * 0.36).clamp(150.0, 260.0);
    let control_left = left + label_width;
    let control = |offset: f32, width: f32| {
        Rect::new(
            control_left,
            top + offset,
            control_left + width,
            top + offset + 24.0,
        )
    };
    SettingsLayout {
        tabs,
        top,
        scan_toggle: control(0.0, 120.0),
        watch_toggle: control(SETTINGS_ROW_HEIGHT, 120.0),
        follow_toggle: control(SETTINGS_ROW_HEIGHT * 2.0, 120.0),
        dedupe_toggle: control(SETTINGS_ROW_HEIGHT * 3.0, 220.0),
        ext_label: Rect::new(left, top + 148.0, control_left - 12.0, top + 174.0),
        ext_edit: Rect::new(control_left, top + 148.0, right - 160.0, top + 174.0),
        add_folder: Rect::new(right - 150.0, top + 148.0, right, top + 172.0),
        folders: Rect::new(left, top + 194.0, right, generic_bottom),
        generic: Rect::new(left, top, right - 12.0, generic_bottom),
        scrollbar: Rect::new(right - 10.0, top, right, generic_bottom),
        label_width,
    }
}

/// Rectangles for the playback-statistics page.
pub struct StatisticsLayout {
    pub refresh: Rect,
    pub reset: Rect,
    /// Eight summary cards (2 rows × 4 columns).
    pub summary: [Rect; 8],
    /// Recently played list and its heading.
    pub recent: Rect,
    pub recent_header: Rect,
    /// Top artists / top tracks columns and headings.
    pub artists: Rect,
    pub artists_header: Rect,
    pub tracks: Rect,
    pub tracks_header: Rect,
}

pub fn statistics(layout: &Layout) -> StatisticsLayout {
    let left = layout.content.left + 24.0;
    let right = layout.content.right - 24.0;
    let top = layout.header.bottom + 8.0;
    let bottom = layout.player.top - 12.0;
    let button_height = 26.0;
    let reset = Rect::new(right - 104.0, top, right, top + button_height);
    let refresh = Rect::new(reset.left - 8.0 - 76.0, top, reset.left - 8.0, top + button_height);
    let grid_top = top + button_height + 10.0;
    let gap = 8.0;
    let column = (right - left - gap * 3.0) / 4.0;
    let card_height = 54.0;
    let mut summary = [Rect::default(); 8];
    for (index, card) in summary.iter_mut().enumerate() {
        let row = index / 4;
        let col = index % 4;
        let card_left = left + col as f32 * (column + gap);
        let card_top = grid_top + row as f32 * (card_height + gap);
        *card = Rect::new(card_left, card_top, card_left + column, card_top + card_height);
    }
    // Leave room for the section headings above the lists; `lists_top - 24`
    // must stay below the second summary row.
    let lists_top = grid_top + card_height * 2.0 + gap + 32.0;
    let lists_bottom = bottom.max(lists_top + 80.0);
    let recent_width = ((right - left - 12.0) * 0.46).max(180.0);
    let recent = Rect::new(left, lists_top, left + recent_width, lists_bottom);
    let recent_header = Rect::new(recent.left, recent.top - 24.0, recent.right, recent.top);
    let side_left = recent.right + 12.0;
    let side_height = (lists_bottom - lists_top - 10.0) * 0.5;
    let artists = Rect::new(side_left, lists_top, right, lists_top + side_height);
    let artists_header = Rect::new(side_left, lists_top - 24.0, right, lists_top);
    let tracks = Rect::new(side_left, artists.bottom + 10.0 + 24.0, right, lists_bottom);
    let tracks_header = Rect::new(
        side_left,
        artists.bottom + 10.0,
        right,
        artists.bottom + 10.0 + 24.0,
    );
    StatisticsLayout {
        refresh,
        reset,
        summary,
        recent,
        recent_header,
        artists,
        artists_header,
        tracks,
        tracks_header,
    }
}

/// One generic settings row (label on the left, control on the right).
/// Uses the `generic` viewport so controls always stop before the reserved
/// scrollbar strip; otherwise the bar overlaps the rightmost "+" button.
pub fn setting_row(settings: &SettingsLayout, index: usize) -> Rect {
    let top = settings.top + index as f32 * SETTINGS_ROW_HEIGHT;
    Rect::new(
        settings.generic.left,
        top,
        settings.generic.right,
        top + 24.0,
    )
}

pub fn setting_control(settings: &SettingsLayout, index: usize) -> Rect {
    let row = setting_row(settings, index);
    // Keep controls compact and right-aligned instead of stretching a stepper
    // across the whole row. At the minimum window width the label column wins
    // and the control shrinks to the remaining space.
    let max_width = (row.width() - settings.label_width - 20.0).max(80.0);
    let width = 320.0_f32.min(max_width);
    Rect::new(
        row.right - width,
        row.top,
        row.right,
        row.bottom,
    )
}

/// Label cell for one generic setting row.
pub fn setting_label(settings: &SettingsLayout, index: usize) -> Rect {
    let row = setting_row(settings, index);
    Rect::new(
        row.left,
        row.top,
        row.left + (settings.label_width - 12.0).max(60.0),
        row.bottom,
    )
}

/// Split a control into equal-width segmented options with a 4 DIP gap.
pub fn segments(area: Rect, count: usize) -> Vec<Rect> {
    let count = count.max(1);
    let gap = 4.0;
    let width = (area.width() - gap * (count.saturating_sub(1)) as f32) / count as f32;
    (0..count)
        .map(|index| {
            let left = area.left + index as f32 * (width + gap);
            Rect::new(left, area.top, left + width, area.bottom)
        })
        .collect()
}

/// `[−]  value  [+]` inside a control rectangle.
pub fn step_rects(area: Rect) -> (Rect, Rect, Rect) {
    let button = 26.0;
    let minus = Rect::new(area.left, area.top, area.left + button, area.bottom);
    let plus = Rect::new(area.right - button, area.top, area.right, area.bottom);
    let value = Rect::new(minus.right + 4.0, area.top, plus.left - 4.0, area.bottom);
    (minus, value, plus)
}

/// The three dedupe modes shown as a segmented control.
pub fn dedupe_options(settings: &SettingsLayout) -> [Rect; 3] {
    let total = settings.dedupe_toggle.width();
    let gap = 4.0;
    let width = (total - gap * 2.0) / 3.0;
    let top = settings.dedupe_toggle.top;
    let bottom = settings.dedupe_toggle.bottom;
    [
        Rect::new(settings.dedupe_toggle.left, top, settings.dedupe_toggle.left + width, bottom),
        Rect::new(
            settings.dedupe_toggle.left + width + gap,
            top,
            settings.dedupe_toggle.left + width * 2.0 + gap,
            bottom,
        ),
        Rect::new(
            settings.dedupe_toggle.left + width * 2.0 + gap * 2.0,
            top,
            settings.dedupe_toggle.left + total,
            bottom,
        ),
    ]
}

/// Native sort LISTBOX (child window) placed under the sort button.
pub fn sort_menu_rect(layout: &Layout) -> Rect {
    let width = 200.0;
    let top = layout.sort.bottom + 4.0;
    // A little extra room so selecting the last item never scrolls the list
    // (a viewport 2px short makes the listbox jump by one full item).
    let full = SORT_ITEM_COUNT as f32 * SORT_ITEM_HEIGHT + 2.0;
    // Never cover the player bar; when the window is short the LISTBOX shows
    // its native scrollbar instead of overflowing.
    let available = (layout.player.top - 8.0 - top).max(SORT_ITEM_HEIGHT * 2.0);
    Rect::new(
        layout.sort.left,
        top,
        layout.sort.left + width,
        top + full.min(available),
    )
}

/// Bounding rect of the folder toolbar (both rows, address bar included).
/// It sits below `header`, so the paint pass must test this rect separately.
pub fn folder_toolbar(layout: &Layout) -> Rect {
    Rect::new(
        layout.folder_back.left,
        layout.folder_back.top,
        layout.folder_address.right,
        layout.folder_play_all.bottom,
    )
}

/// Dirty test region for the whole library control block: the search/sort row
/// plus the header action buttons.
pub fn library_controls(layout: &Layout) -> Rect {
    Rect::new(
        layout.search.left - 4.0,
        14.0,
        layout.header.right + 4.0,
        layout.library_toolbar.bottom + 4.0,
    )
}

/// Dirty test region for the queue filter frame (it sits below the header,
/// inside `layout.list`). Must cover the whole filter, not just its top edge:
/// otherwise a list-only invalidation (row covers, filtering) fills the strip
/// with the background and the frame is never redrawn.
pub fn queue_toolbar(layout: &Layout) -> Rect {
    Rect::new(
        layout.queue_filter.left - 4.0,
        layout.queue_filter.top - 4.0,
        layout.header.right + 4.0,
        layout.queue_filter.bottom + 4.0,
    )
}

pub fn folder_row(settings: &SettingsLayout, index: usize) -> Rect {
    let top = settings.folders.top + index as f32 * FOLDER_ROW_HEIGHT;
    Rect::new(
        settings.folders.left,
        top,
        settings.folders.right,
        top + FOLDER_ROW_HEIGHT - 2.0,
    )
}

pub fn compute(width: f32, height: f32) -> Layout {
    compute_with(width, height, false, false)
}

/// `big_lyrics` hides the song card and lets the lyrics panel fill the page.
/// `visualizer` reserves a strip between the card and the lyrics panel.
pub fn compute_with(width: f32, height: f32, big_lyrics: bool, visualizer_visible: bool) -> Layout {
    let width = width.max(640.0);
    let height = height.max(420.0);
    let body_height = (height - PLAYER_HEIGHT).max(1.0);

    let sidebar = Rect::new(0.0, 0.0, SIDEBAR_WIDTH, body_height);
    let logo = Rect::new(20.0, 20.0, SIDEBAR_WIDTH - 16.0, 48.0);
    let mut nav = [Rect::default(); NAV_ITEMS];
    // Keep the historical 38 DIP rhythm and reserve the footer row. When the
    // window is too short for all 11 items, the list scrolls inside
    // `nav_viewport` instead of squeezing/overlapping the version label.
    let nav_top = 76.0;
    let footer_top = (body_height - 42.0).max(nav_top + 1.0);
    let nav_viewport = Rect::new(6.0, nav_top, SIDEBAR_WIDTH - 6.0, footer_top);
    let step = 38.0;
    let item_height = 34.0;
    for (index, item) in nav.iter_mut().enumerate() {
        let top = nav_top + index as f32 * step;
        *item = Rect::new(12.0, top, SIDEBAR_WIDTH - 12.0, top + item_height);
    }
    let nav_content_height = NAV_ITEMS as f32 * step;
    let nav_scrollbar = if nav_content_height > nav_viewport.height() + 0.5 {
        Rect::new(
            SIDEBAR_WIDTH - 8.0,
            nav_top,
            SIDEBAR_WIDTH - 4.0,
            footer_top,
        )
    } else {
        Rect::default()
    };

    let content = Rect::new(SIDEBAR_WIDTH, 0.0, width, body_height);
    let header = Rect::new(
        content.left + 24.0,
        0.0,
        content.right - 24.0,
        HEADER_HEIGHT,
    );
    let list_top = HEADER_HEIGHT;
    let queue_button = 76.0;
    let queue_gap = 8.0;
    let queue_top = 18.0;
    let queue_clear = Rect::new(
        header.right - queue_button,
        queue_top,
        header.right,
        queue_top + 28.0,
    );
    let queue_export = Rect::new(
        queue_clear.left - queue_gap - queue_button,
        queue_top,
        queue_clear.left - queue_gap,
        queue_top + 28.0,
    );
    let queue_import = Rect::new(
        queue_export.left - queue_gap - queue_button,
        queue_top,
        queue_export.left - queue_gap,
        queue_top + 28.0,
    );
    // The queue filter gets its own toolbar row: the header already carries
    // the page title/subtitle on the left and the shortcut buttons on the
    // right, so sharing that row clips the subtitle.
    let queue_filter = Rect::new(
        content.left + 16.0,
        HEADER_HEIGHT + 6.0,
        content.right - SCROLLBAR_WIDTH - 16.0,
        HEADER_HEIGHT + 32.0,
    );
    let folder_left = content.left + 24.0;
    let folder_top = HEADER_HEIGHT + 8.0;
    let folder_back = Rect::new(folder_left, folder_top, folder_left + 52.0, folder_top + 28.0);
    let folder_forward = Rect::new(
        folder_back.right + 6.0,
        folder_top,
        folder_back.right + 6.0 + 52.0,
        folder_top + 28.0,
    );
    let folder_up = Rect::new(
        folder_forward.right + 6.0,
        folder_top,
        folder_forward.right + 6.0 + 64.0,
        folder_top + 28.0,
    );
    let folder_refresh = Rect::new(
        folder_up.right + 6.0,
        folder_top,
        folder_up.right + 6.0 + 60.0,
        folder_top + 28.0,
    );
    // Second toolbar row: quick actions on the left, address bar filling the
    // remaining width. The bar is the only control that benefits from extra
    // space, so it absorbs all of it (including at the 640 DIP minimum).
    let folder_row = folder_top + 28.0 + 6.0;
    let folder_play_all = Rect::new(folder_left, folder_row, folder_left + 86.0, folder_row + 28.0);
    let folder_queue_all = Rect::new(
        folder_play_all.right + 8.0,
        folder_row,
        folder_play_all.right + 8.0 + 86.0,
        folder_row + 28.0,
    );
    let folder_list_right = content.right - 24.0 - SCROLLBAR_WIDTH - 8.0;
    let folder_address = Rect::new(
        folder_queue_all.right + 8.0,
        folder_row,
        folder_list_right,
        folder_row + 28.0,
    );
    let folder_list = Rect::new(
        folder_left,
        folder_row + 28.0 + 8.0,
        folder_list_right,
        body_height - 10.0,
    );
    let folder_scrollbar = Rect::new(
        folder_list.right + 4.0,
        folder_list.top,
        folder_list.right + 4.0 + SCROLLBAR_WIDTH,
        folder_list.bottom,
    );
    let list = Rect::new(
        content.left + 16.0,
        list_top + 6.0,
        content.right - SCROLLBAR_WIDTH - 16.0,
        body_height - 10.0,
    );
    let queue_list = Rect::new(
        list.left,
        // Leave a visible gap below the filter frame: a partially scrolled
        // first row must not look like it is drawn inside the input's border.
        HEADER_HEIGHT + 44.0,
        list.right,
        body_height - 10.0,
    );
    // Online search: one toolbar row that spans the content width, so the
    // native EDIT is not squeezed next to the header title.
    let online_top = HEADER_HEIGHT + 6.0;
    let online_bottom = online_top + 28.0;
    let online_button = 82.0;
    let online_gap = 6.0;
    let online_random = Rect::new(
        content.right - 24.0 - online_button,
        online_top,
        content.right - 24.0,
        online_bottom,
    );
    let online_queue_all = Rect::new(
        online_random.left - online_gap - online_button,
        online_top,
        online_random.left - online_gap,
        online_bottom,
    );
    let online_kind = Rect::new(
        online_queue_all.left - online_gap - online_button,
        online_top,
        online_queue_all.left - online_gap,
        online_bottom,
    );
    let online_search = Rect::new(
        content.left + 24.0,
        online_top,
        (online_kind.left - 10.0).max(content.left + 64.0),
        online_bottom,
    );
    let online_list = Rect::new(
        content.left + 16.0,
        online_bottom + 8.0,
        content.right - SCROLLBAR_WIDTH - 16.0,
        body_height - 10.0 - PAGER_HEIGHT,
    );
    let online_scrollbar = Rect::new(
        content.right - SCROLLBAR_WIDTH - 8.0,
        online_list.top,
        content.right - 8.0,
        online_list.bottom,
    );
    // Download manager: two header shortcuts and a plain virtual list.
    let download_top = HEADER_HEIGHT + 6.0;
    let download_button = 104.0;
    let download_gap = 8.0;
    let download_clear_finished = Rect::new(
        content.right - 24.0 - download_button,
        download_top,
        content.right - 24.0,
        download_top + 28.0,
    );
    let download_clean_part = Rect::new(
        download_clear_finished.left - download_gap - download_button,
        download_top,
        download_clear_finished.left - download_gap,
        download_top + 28.0,
    );
    let download_list = Rect::new(
        content.left + 16.0,
        download_top + 28.0 + 8.0,
        content.right - SCROLLBAR_WIDTH - 16.0,
        body_height - 10.0,
    );
    let download_scrollbar = Rect::new(
        content.right - SCROLLBAR_WIDTH - 8.0,
        download_list.top,
        content.right - 8.0,
        download_list.bottom,
    );
    // Discover: a single refresh shortcut in the header and a plain list.
    let discover_refresh = Rect::new(
        header.right - 104.0,
        18.0,
        header.right,
        46.0,
    );
    let discover_list = Rect::new(
        content.left + 16.0,
        HEADER_HEIGHT + 6.0,
        content.right - SCROLLBAR_WIDTH - 16.0,
        body_height - 10.0,
    );
    let discover_scrollbar = Rect::new(
        content.right - SCROLLBAR_WIDTH - 8.0,
        discover_list.top,
        content.right - 8.0,
        discover_list.bottom,
    );
    let scrollbar = Rect::new(
        content.right - SCROLLBAR_WIDTH - 8.0,
        queue_list.top,
        content.right - 8.0,
        queue_list.bottom,
    );
    // Library pages instead of scrolling: reserve a pager bar at the bottom
    // and page exactly the rows that fit above it.
    let library_pager = Rect::new(
        content.left + 16.0,
        body_height - 10.0 - PAGER_HEIGHT,
        content.right - 16.0,
        body_height - 10.0,
    );
    // Library: one row below the header with the search box plus the sort
    // chooser; the four whole-library / maintenance actions sit in the header
    // top-right, sized like the queue page's header buttons.
    let sort = Rect::new(
        list.right - 130.0,
        HEADER_HEIGHT + 6.0,
        list.right,
        HEADER_HEIGHT + 34.0,
    );
    let search = Rect::new(
        list.left,
        HEADER_HEIGHT + 6.0,
        sort.left - 8.0,
        HEADER_HEIGHT + 34.0,
    );
    let library_toolbar = Rect::new(
        list.left,
        HEADER_HEIGHT + 2.0,
        list.right,
        HEADER_HEIGHT + 38.0,
    );
    let action_gap = 6.0;
    let action_width = 84.0;
    let group_gap = 16.0;
    let library_check_missing = Rect::new(
        header.right - action_width,
        18.0,
        header.right,
        46.0,
    );
    let library_queue_all = Rect::new(
        library_check_missing.left - action_gap - action_width,
        18.0,
        library_check_missing.left - action_gap,
        46.0,
    );
    let rescan = Rect::new(
        library_queue_all.left - group_gap - action_width,
        18.0,
        library_queue_all.left - group_gap,
        46.0,
    );
    let add_folder = Rect::new(
        rescan.left - action_gap - action_width,
        18.0,
        rescan.left - action_gap,
        46.0,
    );
    let library_list = Rect::new(
        list.left,
        HEADER_HEIGHT + 44.0,
        list.right,
        library_pager.top - 8.0,
    );
    let pager_mid = (library_pager.top + library_pager.bottom) * 0.5;
    let pager_button = 88.0;
    let library_prev = Rect::new(
        library_pager.left,
        pager_mid - 15.0,
        library_pager.left + pager_button,
        pager_mid + 15.0,
    );
    let library_next = Rect::new(
        library_prev.right + 10.0 + PAGE_LABEL_WIDTH + 8.0,
        pager_mid - 15.0,
        library_prev.right + 10.0 + PAGE_LABEL_WIDTH + 8.0 + pager_button,
        pager_mid + 15.0,
    );
    let library_page_label = Rect::new(
        library_prev.right + 10.0,
        library_pager.top,
        library_next.left - 8.0,
        library_pager.bottom,
    );
    let library_page_size = if library_pager.width() >= 470.0 {
        Rect::new(
            library_pager.right - 110.0,
            library_pager.top,
            library_pager.right,
            library_pager.bottom,
        )
    } else {
        Rect::default()
    };
    let online_wall = Rect::new(
        library_pager.right - 96.0,
        library_pager.top + 8.0,
        library_pager.right,
        library_pager.top + 38.0,
    );

    let player = Rect::new(0.0, body_height, width, height);
    let cover = Rect::new(18.0, player.top + 12.0, 66.0, player.top + 60.0);
    let track_text = Rect::new(
        cover.right + 12.0,
        player.top + 10.0,
        (width * 0.5 - 176.0).max(cover.right + 110.0),
        player.top + 60.0,
    );

    // Transport: shuffle / prev / play / next / repeat, centered. The play
    // button is slightly larger than its neighbours, centered on the same row.
    let center = width * 0.5;
    let icon = 28.0;
    let play_icon = 34.0;
    let icon_gap = 8.0;
    let row_top = player.top + 18.0;
    let row_center = row_top + icon * 0.5;
    let play = Rect::new(
        center - play_icon * 0.5,
        row_center - play_icon * 0.5,
        center + play_icon * 0.5,
        row_center + play_icon * 0.5,
    );
    let prev = Rect::new(
        play.left - icon_gap - icon,
        row_top,
        play.left - icon_gap,
        row_top + icon,
    );
    let next = Rect::new(
        play.right + icon_gap,
        row_top,
        play.right + icon_gap + icon,
        row_top + icon,
    );
    let shuffle = Rect::new(prev.left - icon_gap - icon, row_top, prev.left - icon_gap, row_top + icon);
    let repeat = Rect::new(next.right + icon_gap, row_top, next.right + icon_gap + icon, row_top + icon);

    // Right cluster, laid out right-to-left: playlist, desktop lyrics, mini,
    // settings, mute, volume slider.
    let mut right = width - 18.0;
    let playlist = Rect::new(right - icon, row_top, right, row_top + icon);
    right -= icon + 6.0;
    let show_extra = width >= 900.0;
    let desktop_lyrics = if show_extra {
        let rect = Rect::new(right - icon, row_top, right, row_top + icon);
        right -= icon + 6.0;
        rect
    } else {
        Rect::default()
    };
    let mini_player = if show_extra {
        let rect = Rect::new(right - icon, row_top, right, row_top + icon);
        right -= icon + 6.0;
        rect
    } else {
        Rect::default()
    };
    let player_settings = Rect::new(right - icon, row_top, right, row_top + icon);
    right -= icon + 6.0;
    let mute = Rect::new(right - icon, row_top, right, row_top + icon);
    right -= icon + 6.0;
    let volume_slider = if width >= 820.0 {
        let rect = Rect::new(right - 70.0, player.top + 32.0, right, player.top + 36.0);
        right -= 76.0;
        rect
    } else {
        Rect::default()
    };
    let _ = right;
    let volume_hit = if volume_slider.width() > 0.0 {
        volume_slider.inflate(8.0)
    } else {
        Rect::default()
    };

    let progress = Rect::new(
        78.0,
        player.top + 74.0,
        width - 78.0,
        player.top + 78.0,
    );
    let progress_hit = progress.inflate(10.0);
    let time_left = Rect::new(18.0, player.top + 64.0, 70.0, player.top + 84.0);
    let time_right = Rect::new(width - 70.0, player.top + 64.0, width - 18.0, player.top + 84.0);

    // Flyout sits above the gear; the child window is raised above siblings.
    // Narrow windows hide the desktop-lyrics/mini-player buttons from the
    // player bar, so the panel grows to show them (disabled until wired) and
    // carries a volume slider because the bar hides its slider below 820 DIP.
    let panel_width = 260.0;
    let panel_height = if show_extra { 208.0 } else { 264.0 };
    let panel_top = (player.top - panel_height - 6.0).max(header.bottom + 8.0);
    let player_panel = Rect::new(
        (player_settings.right - panel_width).max(8.0),
        panel_top,
        (player_settings.right - panel_width).max(8.0) + panel_width,
        panel_top + panel_height,
    );

    let panel_left = content.left + 28.0;
    let panel_right = content.right - 28.0;
    let bottom_limit = body_height - 12.0;

    let mut card = Rect::default();
    let mut card_cover = Rect::default();
    let mut card_title = Rect::default();
    let mut card_meta = Rect::default();
    let mut card_badges = Rect::default();
    let mut card_buttons = [Rect::default(); 2];

    let mut lyric_panel = if big_lyrics {
        Rect::new(panel_left, header.bottom + 10.0, panel_right, bottom_limit)
    } else {
        let card_top = header.bottom + 10.0;
        let available = (bottom_limit - card_top).max(120.0);
        let card_height = (available * 0.36)
            .clamp(110.0, 210.0)
            .min(available * 0.45);
        card = Rect::new(panel_left, card_top, panel_right, card_top + card_height);
        let pad = 16.0;
        let compact = card_height < 150.0;
        let cover_size = (card_height - pad * 2.0)
            .max(64.0)
            .min(card.width() * 0.34)
            .min(170.0);
        card_cover = Rect::new(
            card.left + pad,
            card.top + pad,
            card.left + pad + cover_size,
            card.top + pad + cover_size,
        );
        let text_left = card_cover.right + 20.0;
        let text_right = card.right - pad;
        if compact {
            card_title = Rect::new(text_left, card.top + 12.0, text_right, card.top + 42.0);
            card_meta = Rect::new(text_left, card.top + 44.0, text_right, card.top + 68.0);
        } else {
            card_title = Rect::new(text_left, card.top + 16.0, text_right, card.top + 52.0);
            card_meta = Rect::new(text_left, card.top + 54.0, text_right, card.top + 78.0);
            card_badges = Rect::new(text_left, card.top + 82.0, text_right, card.top + 104.0);
        }
        let button_height = if compact { 24.0 } else { 28.0 };
        let button_top = card.bottom - 12.0 - button_height;
        let button_width = ((text_right - text_left - 8.0) * 0.5).clamp(60.0, 96.0);
        card_buttons = [
            Rect::new(
                text_left,
                button_top,
                text_left + button_width,
                button_top + button_height,
            ),
            Rect::new(
                text_left + button_width + 8.0,
                button_top,
                text_left + button_width * 2.0 + 8.0,
                button_top + button_height,
            ),
        ];
        Rect::new(panel_left, card.bottom + 12.0, panel_right, bottom_limit)
    };

    // Visualizer strip: sits between the card and the lyrics panel. It is
    // only reserved on the Now Playing page; big-lyrics mode keeps the whole
    // page for the lyrics.
    let mut visualizer = Rect::default();
    if visualizer_visible && !big_lyrics {
        let top = card.bottom + 8.0;
        let bottom = (top + VISUALIZER_HEIGHT).min(bottom_limit - 120.0);
        if bottom - top >= 32.0 {
            visualizer = Rect::new(panel_left, top, panel_right, bottom);
            lyric_panel.top = bottom + 8.0;
        }
    }

    let header_inner = Rect::new(
        lyric_panel.left + 16.0,
        lyric_panel.top + 8.0,
        lyric_panel.right - 16.0,
        lyric_panel.top + 40.0,
    );
    let widths = [56.0f32, 56.0, 48.0, 40.0, 40.0];
    let button_gap = 6.0;
    let mut lyric_buttons = [Rect::default(); LYRIC_BUTTON_COUNT];
    let mut right = header_inner.right;
    for index in (0..LYRIC_BUTTON_COUNT).rev() {
        let left = right - widths[index];
        lyric_buttons[index] = Rect::new(
            left,
            header_inner.top + 2.0,
            right,
            header_inner.top + 28.0,
        );
        right = left - button_gap;
    }
    let lyric_header = Rect::new(header_inner.left, header_inner.top, right, header_inner.bottom);
    let lyrics = Rect::new(
        lyric_panel.left + 8.0,
        lyric_panel.top + 46.0,
        lyric_panel.right - 8.0,
        lyric_panel.bottom - 8.0,
    );

    Layout {
        size: (width, height),
        sidebar,
        nav_viewport,
        nav_scrollbar,
        nav_content_height,
        logo,
        nav,
        content,
        header,
        search,
        sort,
        add_folder,
        rescan,
        list,
        queue_list,
        scrollbar,
        library_list,
        library_toolbar,
        library_queue_all,
        library_check_missing,
        library_pager,
        library_prev,
        library_next,
        library_page_label,
        library_page_size,
        queue_import,
        queue_export,
        queue_clear,
        queue_filter,
        online_search,
        online_kind,
        online_queue_all,
        online_random,
        online_list,
        online_scrollbar,
        online_wall,
        download_clean_part,
        download_clear_finished,
        download_list,
        download_scrollbar,
        discover_refresh,
        discover_list,
        discover_scrollbar,
        folder_back,
        folder_forward,
        folder_up,
        folder_refresh,
        folder_address,
        folder_play_all,
        folder_queue_all,
        folder_list,
        folder_scrollbar,
        player,
        cover,
        track_text,
        shuffle,
        prev,
        play,
        next,
        repeat,
        volume_slider,
        volume_hit,
        mute,
        player_settings,
        mini_player,
        desktop_lyrics,
        playlist,
        player_panel,
        progress,
        progress_hit,
        time_left,
        time_right,
        card,
        card_cover,
        card_title,
        card_meta,
        card_badges,
        card_buttons,
        visualizer,
        lyric_panel,
        lyric_header,
        lyric_buttons,
        lyrics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_tabs_fill_the_row_and_wrap() {
        let layout = compute_with(1200.0, 800.0, false, false);
        let settings = settings(&layout);
        let right = layout.content.right - 32.0;
        // 13 tabs have no divisor above 1, so the flow uses the maximum
        // number of columns that fits (9 here) and wraps the rest.
        assert!(settings.tabs[8].right <= right + 0.5);
        assert!((settings.tabs[8].right - right).abs() < 0.5);
        assert!(settings.tabs[0].width() > 90.0);
        assert!(settings.tabs[9].top > settings.tabs[8].top);
        assert!(settings.tabs[9].left <= settings.tabs[0].left + 0.5);
    }

    #[test]
    fn queue_toolbar_covers_the_whole_filter_frame() {
        let layout = compute_with(1200.0, 800.0, false, false);
        let toolbar = queue_toolbar(&layout);
        assert!(toolbar.height() > 0.0, "{toolbar:?}");
        assert!(toolbar.top <= layout.queue_filter.top);
        assert!(toolbar.bottom >= layout.queue_filter.bottom);
        assert!(toolbar.left <= layout.queue_filter.left);
        assert!(toolbar.right >= layout.queue_filter.right);
        // It must also overlap the generic list rect, because row-cover
        // invalidations use `layout.list` and start at the filter's top edge.
        assert!(toolbar.intersects(layout.list));
    }

    #[test]
    fn sidebar_scrolls_only_when_items_do_not_fit() {
        let small = compute_with(640.0, 460.0, false, false);
        assert!(small.nav_content_height > small.nav_viewport.height());
        assert!(small.nav_scrollbar.width() > 0.0);
        assert!(small.nav_viewport.bottom <= small.sidebar.bottom - 40.0);
        let large = compute_with(1200.0, 900.0, false, false);
        assert!(large.nav_content_height <= large.nav_viewport.height());
        assert_eq!(large.nav_scrollbar.width(), 0.0);
    }

    #[test]
    fn settings_tabs_stay_inside_the_minimum_window() {
        let layout = compute_with(640.0, 420.0, false, false);
        let settings = settings(&layout);
        let left = layout.content.left + 32.0;
        let right = layout.content.right - 32.0;
        for tab in settings.tabs {
            assert!(tab.left >= left - 0.5, "{tab:?}");
            assert!(tab.right <= right + 0.5, "{tab:?}");
        }
        // At 360 DIP usable width the flow falls back to three columns.
        assert!(settings.tabs[3].top > settings.tabs[0].top);
        assert_eq!(settings.tabs[3].top - settings.tabs[0].top, 32.0);
    }

    #[test]
    fn settings_label_column_shrinks_before_controls_collapse() {
        let wide = settings(&compute_with(1400.0, 800.0, false, false));
        let narrow = settings(&compute_with(640.0, 460.0, false, false));
        assert!(wide.label_width >= 240.0);
        assert!(narrow.label_width < wide.label_width);
        let row = setting_row(&narrow, 0);
        let control = setting_control(&narrow, 0);
        assert!(control.left >= row.left + narrow.label_width);
        assert!(control.width() >= 80.0, "{control:?}");
        assert!(
            setting_label(&narrow, 0).right <= control.left + 0.5,
            "labels must not overlap controls"
        );
    }
}
