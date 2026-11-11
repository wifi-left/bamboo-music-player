//! Reusable UI primitives shared by the native pages.
//!
//! Interaction state is kept in one place so new controls only need to:
//! 1. add a [`Target`] variant and its rectangle,
//! 2. feed hover/press/focus updates through [`Interaction`],
//! 3. ask for a [`ButtonState`] when painting.

/// One hoverable/clickable region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Nav(usize),
    /// Scrollbar for the sidebar navigation when it does not fit.
    SidebarScrollbar,
    /// Player bar transport and tools.
    Shuffle,
    Repeat,
    Play,
    Prev,
    Next,
    Volume,
    Mute,
    PlaySettings,
    Playlist,
    MiniPlayer,
    DesktopLyrics,
    Progress,
    Row(usize),
    Scrollbar,
    Sort,
    AddFolder,
    Rescan,
    /// Library toolbar: append the whole result / re-check missing files.
    LibraryQueueAll,
    LibraryCheckMissing,
    /// Queue page header shortcuts.
    QueueImport,
    QueueExport,
    QueueClear,
    /// Folder page toolbar.
    FolderBack,
    FolderForward,
    FolderUp,
    FolderRefresh,
    FolderPlayAll,
    FolderQueueAll,
    /// Library footer pager.
    LibraryPrev,
    LibraryNext,
    SettingToggle(usize),
    SettingDedupeOption(usize),
    SettingAddFolder,
    FolderRemove(usize),
    /// Settings page category and generic rows.
    SettingsTab(usize),
    /// One generic settings row (control area), for keyboard/UIA focus.
    SettingsRow(usize),
    SettingsToggleRow(usize),
    SettingsChoice(usize, usize),
    /// Narrow-window fallback: one cycling button instead of segments.
    SettingsChoiceCycle(usize),
    SettingsStep(usize, i32),
    /// Color swatch row; opens the native color picker.
    SettingsColor(usize),
    /// Action button row (file/font picker or direct action).
    SettingsAction(usize),
    /// Inline text row.
    SettingsText(usize),
    /// Settings page scrollbar for the generic rows.
    SettingsScrollbar,
    /// Now Playing card: play/pause and open-file actions.
    CardPlay,
    CardOpen,
    /// Smart playlist preset / duplicate mode selectors.
    SmartKind,
    DuplicateMode,
    /// Online search page: kind toggle / back, queue-all and random batch.
    OnlineKind,
    OnlineBack,
    OnlineQueueAll,
    OnlineRandom,
    OnlinePrev,
    OnlineNext,
    /// Toggle the album/artist detail cover grid.
    OnlineWall,
    /// Discover page refresh.
    DiscoverRefresh,
    /// Download manager row buttons and toolbar.
    DownloadAction(usize, DownloadAction),
    DownloadsCleanPart,
    DownloadsClearFinished,
    /// Statistics page toolbar and recently-played rows.
    StatsRefresh,
    StatsReset,
    StatsRecent(usize),
    /// Lyrics panel header actions.
    LyricsPick,
    LyricsWindow,
    LyricsBig,
    LyricsEdit,
    LyricsReload,
}

/// Per-row download manager actions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadAction {
    Retry,
    Cancel,
    Open,
    Quality,
}

/// Native text inputs owned by the shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    Search,
    Extensions,
    /// Folder page address bar (path input).
    FolderPath,
    /// Numeric EDIT over a font-size step value.
    SettingNumber,
}

/// Visual state of a button-like control.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonState {
    Normal,
    Hover,
    /// Hovering a button that is currently toggled on: a distinct color from
    /// hovering an "off" button.
    HoverActive,
    Pressed,
    /// Toggled on (e.g. settings "开").
    Active,
}

/// Hover, press and focus bookkeeping for the whole window.
#[derive(Default)]
pub struct Interaction {
    hover: Option<Target>,
    pressed: Option<Target>,
    focus: Option<Input>,
}

impl Interaction {
    pub fn hovered(&self, target: Target) -> bool {
        self.hover == Some(target)
    }

    /// Currently hovered region, if any (tooltips, context menus).
    pub fn hover_target(&self) -> Option<Target> {
        self.hover
    }

    pub fn pressed(&self, target: Target) -> bool {
        self.pressed == Some(target)
    }

    pub fn focused(&self, input: Input) -> bool {
        self.focus == Some(input)
    }

    pub fn button_state(&self, target: Target, active: bool) -> ButtonState {
        if self.pressed(target) {
            ButtonState::Pressed
        } else if self.hovered(target) {
            if active {
                ButtonState::HoverActive
            } else {
                ButtonState::Hover
            }
        } else if active {
            ButtonState::Active
        } else {
            ButtonState::Normal
        }
    }

    /// Replace the hovered target. Returns the old/new targets so the caller
    /// can invalidate exactly the two affected rectangles.
    pub fn set_hover(&mut self, target: Option<Target>) -> (Option<Target>, Option<Target>) {
        let old = self.hover;
        self.hover = target;
        (old, target)
    }

    /// Replace the pressed target with the same old/new contract.
    pub fn set_pressed(&mut self, target: Option<Target>) -> (Option<Target>, Option<Target>) {
        let old = self.pressed;
        self.pressed = target;
        (old, target)
    }

    /// Replace the focused input. Returns the previous value.
    pub fn set_focus(&mut self, focus: Option<Input>) -> Option<Input> {
        let old = self.focus;
        self.focus = focus;
        old
    }

    /// Clear hover and press; returns the targets that need repainting.
    pub fn clear(&mut self) -> (Option<Target>, Option<Target>) {
        let hovered = self.hover.take();
        let pressed = self.pressed.take();
        (hovered, pressed)
    }

    pub fn clear_pressed(&mut self) -> Option<Target> {
        self.pressed.take()
    }
}
