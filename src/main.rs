#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! Bamboo Music Player — native Win32 + Direct2D shell (M1).
//!
//! M1 scope: standard-chrome window, sidebar navigation, a virtualized list,
//! a player bar with a simulated clock, and a KTV line. Painting is driven by
//! `InvalidateRect`: only the rectangles that changed are redrawn, text layouts
//! and brushes are cached, and the animation timer only runs while playing.
//!
//! The archived egui implementation under `ref/archived/bamboo-egui` is
//! reference material only; it is not a dependency of this crate.

mod accessibility;
mod app;
mod core;
mod demo;
mod drop_target;
mod gamepad;
mod layout;
mod lyrics_editor_window;
mod lyrics_screen;
mod lyrics_view;
mod lyrics_window;
mod overlay_host;
mod player_panel;
mod paint;
mod shortcuts;
mod theme;
mod ui;

// The migrated business modules kept their original `crate::model`,
// `crate::config`, ... paths; re-exporting them here keeps the move mechanical.
pub use self::core::{
    api, audio, config, desktop_overlay, download, i18n, library, lyrics, mini_overlay, model,
    overlay_protocol, platform, playlist, radio, util,
};

use std::ffi::OsString;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use windows::Win32::Foundation::{
    COLORREF, GlobalFree, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, EndPaint, FONT_CHARSET,
    FONT_CLIP_PRECISION, FONT_OUTPUT_PRECISION, FONT_QUALITY, GetDC, GetTextMetricsW, HBRUSH, HDC,
    HFONT, HGDIOBJ, InvalidateRect, LOGFONTW, ReleaseDC,
    PAINTSTRUCT, ScreenToClient, SelectObject, SetBkMode, SetTextColor, TEXTMETRICW, TRANSPARENT,
    UpdateWindow,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Ole::{IDropTarget, OleInitialize, OleUninitialize, RevokeDragDrop};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock,
};
use windows::Win32::UI::Controls::Dialogs::{
    CC_ANYCOLOR, CC_FULLOPEN, CC_RGBINIT, CF_FORCEFONTEXIST, CF_INITTOLOGFONTSTRUCT,
    CF_NOVERTFONTS, CF_SCREENFONTS, CHOOSECOLORW, CHOOSEFONTW, ChooseColorW, ChooseFontW,
};
use windows::Win32::UI::Controls::{
    DRAWITEMSTRUCT, ICC_STANDARD_CLASSES, ICC_WIN95_CLASSES, INITCOMMONCONTROLSEX,
    InitCommonControlsEx, ODS_SELECTED, SetWindowTheme, TTF_ABSOLUTE, TTF_TRACK,
    TTM_ADDTOOLW, TTM_TRACKACTIVATE, TTM_TRACKPOSITION, TTM_UPDATETIPTEXTW, TTS_ALWAYSTIP,
    TTS_NOPREFIX, TTTOOLINFOW, WM_MOUSELEAVE,
};
use windows::Win32::Graphics::Dwm::{
    DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem, GetDpiForWindow,
    SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, HOT_KEY_MODIFIERS, RegisterHotKey, SetFocus, TME_LEAVE, TRACKMOUSEEVENT,
    TrackMouseEvent, UnregisterHotKey, VK_CONTROL, VK_ESCAPE, VK_LWIN, VK_MENU, VK_RETURN,
    VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::Shell::{DragAcceptFiles, DragFinish, DragQueryFileW, HDROP, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CW_USEDEFAULT, CallWindowProcW, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
    DestroyMenu, DestroyWindow,
    DispatchMessageW,
    GWLP_USERDATA, GWLP_WNDPROC, GetClientRect, GetCursorPos, GetForegroundWindow,
    GetMessageW, GetParent, GetSystemMetrics,
    GetWindowLongPtrW, GetWindowPlacement, GetWindowRect,
    GetWindowTextW,
    HWND_TOPMOST, HMENU, IDC_ARROW, IDYES, IsIconic, IsWindowVisible, IsZoomed, KillTimer,
    LoadCursorW,
    LoadIconW, MB_ICONERROR, MB_ICONQUESTION, MB_ICONWARNING, MB_OK, MB_YESNO, MF_GRAYED,
    MF_SEPARATOR, MF_STRING, MINMAXINFO,
    MSG, MessageBoxW, MoveWindow, PostMessageW, PostQuitMessage, RegisterClassW,
    SM_CXVIRTUALSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_HIDE, SW_SHOW, SW_SHOWMAXIMIZED,
    SW_SHOWMINIMIZED, SW_SHOWNORMAL, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SendMessageW,
    SetForegroundWindow, SetWindowDisplayAffinity,
    SetTimer, SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow, TrackPopupMenu,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, TranslateMessage,
    UnregisterClassW, WDA_EXCLUDEFROMCAPTURE, WDA_NONE,
    WINDOWPLACEMENT, WNDCLASS_STYLES, WINDOW_EX_STYLE, WINDOW_STYLE, WPF_RESTORETOMAXIMIZED,
    WS_EX_TOPMOST, WS_POPUP,
    WM_ACTIVATE, WM_APP, WM_CLOSE, WM_COMMAND, WM_CONTEXTMENU,
    WM_CTLCOLOREDIT, WM_DESTROY, WM_DEVICECHANGE,
    WM_DPICHANGED, WM_DROPFILES, WM_ERASEBKGND, WM_GETMINMAXINFO, WM_GETOBJECT, WM_HOTKEY,
    WM_KEYDOWN, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_NULL, WM_PAINT, WM_SIZE, WM_SHOWWINDOW, WM_SYSCOMMAND, WM_SYSKEYDOWN, WM_TIMER,
    LB_ADDSTRING, LB_GETCURSEL, LB_ITEMFROMPOINT, LB_SETCURSEL, LB_SETITEMHEIGHT, LBN_SELCHANGE,
    WNDCLASSW, WNDPROC,
    WS_CHILD, WS_CLIPCHILDREN, WS_OVERLAPPEDWINDOW, WS_VISIBLE, WM_CTLCOLORLISTBOX, WM_DRAWITEM,
    WM_SETFONT,
};
use windows::core::{PCWSTR, PWSTR, w};

use crate::app::{App, Page};
use crate::core::model::{RepeatMode, Track, TrackSource};
use crate::core::platform::jump_list;
use crate::core::platform::notify;
use crate::core::platform::registry;
use crate::core::platform::single_instance;
use crate::core::platform::smtc::{SmtcController, SmtcEvent};
use crate::core::platform::tray::{Tray, TrayAction};
use crate::core::platform::ipc;
use crate::layout::Rect;
use crate::lyrics_view::{LyricsChild, WM_APP_LYRIC_MENU, WM_APP_LYRIC_SEEK};
use crate::lyrics_editor_window::{
    LyricsEditorWindow, WM_APP_EDITOR_CLOSED, WM_APP_EDITOR_SAVED,
};
use crate::lyrics_window::{
    LYRICS_ACTION_NEXT, LYRICS_ACTION_PLAY_PAUSE, LYRICS_ACTION_PREV, LyricsWindow,
    WM_APP_LYRICS_ACTION, WM_APP_LYRICS_CLOSED, WM_APP_LYRICS_SYNC,
};
use crate::overlay_host::{DesktopHost, MiniHost};
use crate::overlay_protocol::{MiniAction, OverlayAction};
use crate::gamepad::{Gamepad, GamepadAction};
use crate::player_panel::{
    ACTION_AB_CLEAR, ACTION_AB_END, ACTION_AB_START, ACTION_DEVICE_DELTA, ACTION_OPEN_SETTINGS,
    ACTION_SET_SLEEP, ACTION_SET_VOLUME, ACTION_SLEEP_CUSTOM_DELTA, ACTION_SPEED_DOWN,
    ACTION_SPEED_UP, ACTION_TOGGLE_DESKTOP_LYRICS, ACTION_TOGGLE_EQ, ACTION_TOGGLE_MINI_PLAYER,
    ACTION_TOGGLE_PITCH, PlayerPanel, WM_APP_PLAYER_ACTION,
};
use crate::paint::Painter;

/// Global hotkey ids. The order matches `ShortcutSlot::ALL`.
const HOTKEY_BASE: usize = 0x7100;
const SHORTCUT_COUNT: usize = 7;

const ANIM_TIMER: usize = 1;
const QUIT_TIMER: usize = 2;
/// Debounce for the native library search EDIT.
const SEARCH_TIMER: usize = 3;
const EXTENSIONS_TIMER: usize = 4;
/// Delay before a row tooltip appears (native-like hover feel).
const TOOLTIP_TIMER: usize = 5;
/// Auto-hide a tooltip that stays open while the cursor rests on one row.
const TOOLTIP_HIDE_TIMER: usize = 6;
/// Low-frequency idle check for optional working-set trimming.
const IDLE_TRIM_TIMER: usize = 7;
/// Gamepad button sampling; only runs while a controller is connected.
const GAMEPAD_TIMER: usize = 8;
/// Shell integration (SMTC/notifications/jump list/registry) cadence. It does
/// not need to run at the animation frame rate.
const SYSTEM_SYNC_INTERVAL: Duration = Duration::from_millis(250);
/// Overlay hosts only send a position tick every 250 ms; syncing them at
/// 50 ms keeps state changes responsive without rebuilding a snapshot at 60 Hz.
const OVERLAY_SYNC_INTERVAL: Duration = Duration::from_millis(50);
const ID_SEARCH: usize = 100;
const ID_EXTENSIONS: usize = 101;
const ID_SORT_LIST: usize = 102;
const ID_SETTING_NUMBER: usize = 103;
/// Inline text editor for settings rows (API address, proxy, credentials).
const ID_SETTING_TEXT: usize = 106;
/// Folder page address bar (native EDIT).
const ID_FOLDER_PATH: usize = 104;
/// Posted by the number-EDIT subclass when the user presses Enter / Escape.
const WM_APP_NUMBER_COMMIT: u32 = WM_APP + 11;
const WM_APP_NUMBER_CANCEL: u32 = WM_APP + 12;
/// Posted by the inline text-EDIT subclass on Enter / Escape.
const WM_APP_TEXT_COMMIT: u32 = WM_APP + 16;
const WM_APP_TEXT_CANCEL: u32 = WM_APP + 17;
/// Posted by the folder address-bar subclass on Enter / Escape.
const WM_APP_FOLDER_GO: u32 = WM_APP + 13;
const WM_APP_FOLDER_CANCEL: u32 = WM_APP + 14;

/// Hovered row of the native sort LISTBOX (-1 = none). The subclass below
/// updates it and invalidates the child; no parent state is touched.
static SORT_HOVER: AtomicIsize = AtomicIsize::new(-1);
static SORT_ORIG_PROC: AtomicIsize = AtomicIsize::new(0);

unsafe extern "system" fn sort_list_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_MOUSEMOVE => {
            track_mouse_leave(hwnd);
            let packed = unsafe {
                SendMessageW(hwnd, LB_ITEMFROMPOINT, Some(WPARAM(0)), Some(lparam))
            }
            .0 as u32;
            let outside = (packed >> 16) != 0;
            let value = if outside { -1 } else { (packed & 0xFFFF) as isize };
            if SORT_HOVER.swap(value, Ordering::Relaxed) != value {
                let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
            }
        }
        WM_MOUSELEAVE => {
            if SORT_HOVER.swap(-1, Ordering::Relaxed) != -1 {
                let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
            }
        }
        _ => {}
    }
    let original = SORT_ORIG_PROC.load(Ordering::Relaxed);
    if original == 0 {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    let procedure: WNDPROC = unsafe { std::mem::transmute(original) };
    unsafe { CallWindowProcW(procedure, hwnd, message, wparam, lparam) }
}
const EN_CHANGE: u16 = 0x0300;

static NUMBER_ORIG_PROC: AtomicIsize = AtomicIsize::new(0);

unsafe extern "system" fn number_edit_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_KEYDOWN {
        let key = wparam.0 as u16;
        let parent = unsafe { GetParent(hwnd) }.unwrap_or_default();
        if key == VK_RETURN.0 {
            let _ = unsafe { PostMessageW(Some(parent), WM_APP_NUMBER_COMMIT, WPARAM(0), LPARAM(0)) };
            return LRESULT(0);
        }
        if key == VK_ESCAPE.0 {
            let _ = unsafe { PostMessageW(Some(parent), WM_APP_NUMBER_CANCEL, WPARAM(0), LPARAM(0)) };
            return LRESULT(0);
        }
    }
    let original = NUMBER_ORIG_PROC.load(Ordering::Relaxed);
    if original == 0 {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    let procedure: WNDPROC = unsafe { std::mem::transmute(original) };
    unsafe { CallWindowProcW(procedure, hwnd, message, wparam, lparam) }
}

static TEXT_ROW_ORIG_PROC: AtomicIsize = AtomicIsize::new(0);

unsafe extern "system" fn text_row_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_KEYDOWN {
        let key = wparam.0 as u16;
        let parent = unsafe { GetParent(hwnd) }.unwrap_or_default();
        if key == VK_RETURN.0 {
            let _ =
                unsafe { PostMessageW(Some(parent), WM_APP_TEXT_COMMIT, WPARAM(0), LPARAM(0)) };
            return LRESULT(0);
        }
        if key == VK_ESCAPE.0 {
            let _ =
                unsafe { PostMessageW(Some(parent), WM_APP_TEXT_CANCEL, WPARAM(0), LPARAM(0)) };
            return LRESULT(0);
        }
    }
    let original = TEXT_ROW_ORIG_PROC.load(Ordering::Relaxed);
    if original == 0 {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    let procedure: WNDPROC = unsafe { std::mem::transmute(original) };
    unsafe { CallWindowProcW(procedure, hwnd, message, wparam, lparam) }
}

static FOLDER_PATH_ORIG_PROC: AtomicIsize = AtomicIsize::new(0);

unsafe extern "system" fn folder_path_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_KEYDOWN {
        let key = wparam.0 as u16;
        let parent = unsafe { GetParent(hwnd) }.unwrap_or_default();
        if key == VK_RETURN.0 {
            let _ = unsafe { PostMessageW(Some(parent), WM_APP_FOLDER_GO, WPARAM(0), LPARAM(0)) };
            return LRESULT(0);
        }
        if key == VK_ESCAPE.0 {
            let _ = unsafe { PostMessageW(Some(parent), WM_APP_FOLDER_CANCEL, WPARAM(0), LPARAM(0)) };
            return LRESULT(0);
        }
    }
    let original = FOLDER_PATH_ORIG_PROC.load(Ordering::Relaxed);
    if original == 0 {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    let procedure: WNDPROC = unsafe { std::mem::transmute(original) };
    unsafe { CallWindowProcW(procedure, hwnd, message, wparam, lparam) }
}

/// Posted by the search EDIT subclass on Enter.
pub(crate) const WM_APP_SEARCH_GO: u32 = WM_APP + 15;

static SEARCH_ORIG_PROC: AtomicIsize = AtomicIsize::new(0);

unsafe extern "system" fn search_edit_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_KEYDOWN && wparam.0 as u16 == VK_RETURN.0 {
        let parent = unsafe { GetParent(hwnd) }.unwrap_or_default();
        let _ = unsafe { PostMessageW(Some(parent), WM_APP_SEARCH_GO, WPARAM(0), LPARAM(0)) };
        return LRESULT(0);
    }
    let original = SEARCH_ORIG_PROC.load(Ordering::Relaxed);
    if original == 0 {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    let procedure: WNDPROC = unsafe { std::mem::transmute(original) };
    unsafe { CallWindowProcW(procedure, hwnd, message, wparam, lparam) }
}

/// Posted by the tray/overlay threads to wake the UI thread on demand.
pub(crate) const WM_APP_WAKE: u32 = WM_APP + 1;

struct State {
    app: App,
    painter: Painter,
    lyrics: LyricsChild,
    /// Standalone lyrics window; `None` while it is closed.
    lyrics_window: Option<LyricsWindow>,
    /// Native lyrics editor; `None` while it is closed.
    lyrics_editor: Option<LyricsEditorWindow>,
    panel: PlayerPanel,
    tray: Option<Tray>,
    /// Native overlays; `None` while closed.
    desktop_overlay: Option<DesktopHost>,
    mini_overlay: Option<MiniHost>,
    wake: Arc<dyn Fn() + Send + Sync>,
    smtc: Option<SmtcController>,
    smtc_wanted: bool,
    smtc_failed: bool,
    /// Forwarded command lines from a second instance / jump list.
    ipc: Receiver<Vec<String>>,
    last_notify_key: Option<String>,
    last_jump_key: Option<String>,
    jump_list_on: bool,
    applied_capture: Option<bool>,
    applied_autostart: Option<bool>,
    applied_associations: Option<bool>,
    applied_playlist_assoc: Option<bool>,
    shuffle: bool,
    repeat: RepeatMode,
    bench: Option<f32>,
    painted: bool,
    /// Whether window placement and other session fields may be written.
    /// `--bench` / `--self-test` runs must not touch the user's session.
    persist: bool,
    started: Instant,
    edit: HWND,
    edit_extensions: HWND,
    /// Native numeric EDIT shown over a font-size step value.
    edit_number: HWND,
    /// Native inline text EDIT over a text settings row (API address etc.).
    edit_text_row: HWND,
    /// Folder page address bar (path input; Enter navigates).
    edit_folder: HWND,
    /// Native owner-drawn LISTBOX used as the library sort selector.
    sort_list: HWND,
    /// Native tooltip popup for queue/library row information.
    tooltip: HWND,
    /// Row the tooltip is scheduled for / currently describes.
    tooltip_row: Option<usize>,
    /// Settings row the tooltip describes (long values only).
    tooltip_setting: Option<usize>,
    tooltip_visible: bool,
    /// Keeps the registered OLE drop target alive for the window lifetime.
    drop_target: Option<IDropTarget>,
    edit_font: HFONT,
    edit_brush: HBRUSH,
    list_brush: HBRUSH,
    /// Palette used by the native EDIT/LISTBOX brushes above.
    palette: crate::theme::Palette,
    /// Custom colors remembered by the native color picker.
    custom_colors: [COLORREF; 16],
    /// Last mini-player settings sent to the overlay. A change forces a full
    /// state push even while paused (no animation timer is running).
    mini_key: Option<MiniSettingsKey>,
    /// Same idea for the desktop-lyrics overlay.
    desktop_key: Option<DesktopSettingsKey>,
    last_page: Option<crate::app::Page>,
    /// Last (background mode, dark) pair applied through DWM.
    backdrop: Option<(crate::core::config::BackgroundMode, bool)>,
    /// Last user input; the idle memory trim waits for this to age.
    last_input: Instant,
    last_trim: Instant,
    /// Shell integration is throttled: SMTC, notifications, jump list and
    /// registry mirrors do not need a 60 Hz refresh.
    last_system_sync: Instant,
    /// Overlay position/state pushes are throttled to the host heartbeat; the
    /// overlay windows interpolate locally between messages.
    last_overlay_sync: Instant,
    /// Last global-hotkey registrations, so a settings edit only re-registers
    /// when something actually changed.
    shortcuts_applied: Option<ShortcutsApplied>,
    /// XInput controller; polled only while connected and enabled.
    gamepad: Gamepad,
    gamepad_running: bool,
}

struct Mode {
    self_test: bool,
    bench: Option<f32>,
    page: Option<String>,
    settings_tab: Option<usize>,
    theme: Option<crate::config::ThemeMode>,
    library: Option<std::path::PathBuf>,
    folder: Option<std::path::PathBuf>,
}

/// Content/window fields that change how the mini player renders. Compared
/// once per sync; any difference forces a full state push.
#[derive(Clone, Copy, PartialEq)]
struct MiniSettingsKey {
    show_lyrics: bool,
    karaoke: bool,
    words_only: bool,
    lines: u8,
    bold: bool,
    translation: bool,
    font_size: f32,
    width: f32,
    height: f32,
    auto_height: bool,
    always_on_top: bool,
    opacity: f32,
    taskbar: bool,
    panel_color: [u8; 3],
    text_color: [u8; 3],
    accent_color: [u8; 3],
    d2d_hardware: bool,
}

fn mini_settings_key(app: &App) -> MiniSettingsKey {
    let mini = &app.core.settings.mini;
    MiniSettingsKey {
        show_lyrics: mini.show_lyrics,
        karaoke: mini.karaoke,
        words_only: mini.karaoke_words_only,
        lines: mini.lyrics_lines.clamp(1, 2),
        bold: mini.lyrics_bold,
        translation: mini.show_translation,
        font_size: mini.lyrics_font_size,
        width: mini.width,
        height: mini.height,
        auto_height: mini.auto_height,
        always_on_top: mini.always_on_top,
        opacity: mini.opacity,
        taskbar: mini.show_in_taskbar,
        panel_color: mini.panel_color,
        text_color: mini.text_color,
        accent_color: mini.accent_color,
        d2d_hardware: app.core.settings.performance.d2d_hardware,
    }
}

/// Enabled flag plus the seven combinations currently registered. Order
/// matches `ShortcutSlot::ALL`.
#[derive(Clone, PartialEq, Eq)]
struct ShortcutsApplied {
    enabled: bool,
    combos: [String; SHORTCUT_COUNT],
}

fn shortcuts_applied(app: &App) -> ShortcutsApplied {
    let shortcuts = &app.core.settings.shortcuts;
    let mut combos = std::array::from_fn(|_| String::new());
    for (index, slot) in crate::config::ShortcutSlot::ALL.into_iter().enumerate() {
        combos[index] = shortcuts.get(slot).to_string();
    }
    ShortcutsApplied {
        enabled: shortcuts.enabled,
        combos,
    }
}

/// Desktop-overlay fields exposed by the settings page. A change forces a
/// full state push even while paused (the animation timer is stopped).
#[derive(Clone, PartialEq)]
struct DesktopSettingsKey {
    locked: bool,
    click_through: bool,
    always_on_top: bool,
    lines: u8,
    translation: bool,
    footer: bool,
    font_size: f32,
    font_family: Option<String>,
    bold: bool,
    align: u8,
    background: u8,
    bg_image: Option<std::path::PathBuf>,
    opacity: f32,
    radius: u8,
    padding: f32,
    line_spacing: f32,
    shadow: bool,
    outline: bool,
    border: bool,
    window_shadow: bool,
    auto_size: bool,
    width: f32,
    height: f32,
    hide_when_paused: bool,
    hide_when_fullscreen: bool,
    capture: bool,
    karaoke: bool,
    words_only: bool,
    outline_width: f32,
    shadow_offset: f32,
    translation_outline: bool,
    translation_shadow: bool,
    footer_outline: bool,
    footer_shadow: bool,
    current: [u8; 4],
    next: [u8; 4],
    prev: [u8; 4],
    footer_color: [u8; 4],
    translation_color: [u8; 4],
    outline_color: [u8; 4],
    shadow_color: [u8; 4],
    bg_color: [u8; 4],
    bg_color2: [u8; 4],
    d2d_hardware: bool,
}

fn desktop_settings_key(app: &App) -> DesktopSettingsKey {
    let d = &app.core.settings.desktop_lyrics;
    DesktopSettingsKey {
        locked: d.locked,
        click_through: d.click_through,
        always_on_top: d.always_on_top,
        lines: d.lines.clamp(1, 3),
        translation: d.show_translation,
        footer: d.show_footer,
        font_size: d.font_size,
        font_family: d.font_family.clone(),
        bold: d.bold,
        align: match d.align {
            crate::config::LyricsAlign::Left => 0,
            crate::config::LyricsAlign::Center => 1,
            crate::config::LyricsAlign::Right => 2,
        },
        background: match d.background {
            crate::config::DesktopBackgroundMode::Transparent => 0,
            crate::config::DesktopBackgroundMode::Solid => 1,
            crate::config::DesktopBackgroundMode::Gradient => 2,
            crate::config::DesktopBackgroundMode::Image => 3,
            crate::config::DesktopBackgroundMode::BlurCover => 4,
        },
        bg_image: d.bg_image.clone(),
        opacity: d.opacity,
        radius: d.radius,
        padding: d.padding,
        line_spacing: d.line_spacing,
        shadow: d.shadow,
        outline: d.outline,
        border: d.border,
        window_shadow: d.window_shadow,
        auto_size: d.auto_size,
        width: d.width,
        height: d.height,
        hide_when_paused: d.hide_when_paused,
        hide_when_fullscreen: d.hide_when_fullscreen,
        capture: d.capture_protection,
        karaoke: d.karaoke,
        words_only: d.karaoke_words_only,
        outline_width: d.outline_width,
        shadow_offset: d.shadow_offset,
        translation_outline: d.translation_outline,
        translation_shadow: d.translation_shadow,
        footer_outline: d.footer_outline,
        footer_shadow: d.footer_shadow,
        current: d.current_color,
        next: d.next_color,
        prev: d.prev_color,
        footer_color: d.footer_color,
        translation_color: d.translation_color,
        outline_color: d.outline_color,
        shadow_color: d.shadow_color,
        bg_color: d.bg_color,
        bg_color2: d.bg_color2,
        d2d_hardware: app.core.settings.performance.d2d_hardware,
    }
}

fn main() -> ExitCode {
    init_logging();
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            eprintln!("bamboo-music-player: fatal error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}

fn parse_mode() -> Mode {
    let args: Vec<String> = std::env::args().collect();
    let mut self_test = args.iter().any(|arg| arg == "--self-test");
    let mut bench = None;
    if let Some(position) = args.iter().position(|arg| arg == "--bench") {
        let seconds = args
            .get(position + 1)
            .and_then(|value| value.parse::<f32>().ok())
            .unwrap_or(10.0)
            .clamp(1.0, 600.0);
        bench = Some(seconds);
    } else if let Some(argument) = args.iter().find(|arg| arg.starts_with("--bench=")) {
        bench = Some(
            argument[8..]
                .parse::<f32>()
                .unwrap_or(10.0)
                .clamp(1.0, 600.0),
        );
    }
    if bench.is_some() {
        self_test = false;
    }
    let page = args
        .iter()
        .position(|arg| arg == "--page")
        .and_then(|position| args.get(position + 1).cloned())
        .or_else(|| {
            args.iter()
                .find_map(|arg| arg.strip_prefix("--page=").map(str::to_string))
        });
    let settings_tab = args
        .iter()
        .position(|arg| arg == "--settings-tab")
        .and_then(|position| args.get(position + 1))
        .and_then(|value| value.parse::<usize>().ok())
        .or_else(|| {
            args.iter()
                .find_map(|arg| arg.strip_prefix("--settings-tab="))
                .and_then(|value| value.parse::<usize>().ok())
        });
    let theme = args
        .iter()
        .position(|arg| arg == "--theme")
        .and_then(|position| args.get(position + 1))
        .map(String::as_str)
        .map(|value| match value {
            "light" => crate::config::ThemeMode::Light,
            "system" => crate::config::ThemeMode::System,
            _ => crate::config::ThemeMode::Dark,
        })
        .or_else(|| {
            args.iter().find_map(|arg| {
                arg.strip_prefix("--theme=").map(|value| match value {
                    "light" => crate::config::ThemeMode::Light,
                    "system" => crate::config::ThemeMode::System,
                    _ => crate::config::ThemeMode::Dark,
                })
            })
        });
    let library = args
        .iter()
        .position(|arg| arg == "--library")
        .and_then(|position| args.get(position + 1))
        .map(std::path::PathBuf::from)
        .or_else(|| {
            args.iter()
                .find_map(|arg| arg.strip_prefix("--library=").map(std::path::PathBuf::from))
        });
    let folder = args
        .iter()
        .position(|arg| arg == "--folder")
        .and_then(|position| args.get(position + 1))
        .map(std::path::PathBuf::from)
        .or_else(|| {
            args.iter()
                .find_map(|arg| arg.strip_prefix("--folder=").map(std::path::PathBuf::from))
        });
    Mode {
        self_test,
        bench,
        page,
        settings_tab,
        theme,
        library,
        folder,
    }
}

fn run() -> windows::core::Result<u8> {
    let mode = parse_mode();

    // Single-instance guard runs before any window exists. A second launch
    // forwards its command line (files, jump-list actions) to the running
    // player and exits; `--bench`/`--self-test` stay independent.
    if mode.bench.is_none() && !mode.self_test {
        let paths = crate::core::config::Paths::discover();
        let settings = crate::core::config::load_settings(&paths.settings_file);
        if !settings.window.allow_multiple_instances && single_instance::already_running() {
            let args: Vec<OsString> = std::env::args_os()
                .skip(1)
                .filter(|arg| {
                    let text = arg.to_string_lossy();
                    !text.starts_with("--bench") && !text.starts_with("--self-test")
                })
                .collect();
            let forwarded = ipc::send_to_existing(&args);
            info!("another instance is running (forwarded={forwarded}); exiting");
            return Ok(0);
        }
    }

    unsafe {
        if let Err(err) = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
        {
            warn!("per-monitor DPI awareness unavailable: {err}");
        }
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        // OLE is required for RegisterDragDrop (live drag preview + standard
        // drag image). It shares the STA initialized above.
        if let Err(err) = OleInitialize(None) {
            warn!("OLE initialization unavailable: {err}");
        }
        // MFC-style visual styles: the manifest already pulls in
        // Microsoft.Windows.Common-Controls 6.0.0.0; this registers the
        // standard/Win95 classes before any child window is created.
        let common_controls = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_STANDARD_CLASSES | ICC_WIN95_CLASSES,
        };
        let _ = InitCommonControlsEx(&common_controls);

        let hinstance = HINSTANCE(GetModuleHandleW(None)?.0);
        let class_name = w!("BambooMusicPlayerNative");
        // The icon is embedded by build.rs with resource id 1; the class needs
        // it explicitly, otherwise the title bar shows the generic icon.
        let icon = LoadIconW(Some(hinstance), windows::core::PCWSTR(1 as *const u16))
            .unwrap_or_default();
        let window_class = WNDCLASSW {
            // No CS_DBLCLKS: every press is a normal click, so rapid
            // next/play/shuffle clicks are never swallowed by the system
            // double-click window.
            style: WNDCLASS_STYLES(0),
            hInstance: hinstance,
            hIcon: icon,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hbrBackground: HBRUSH(std::ptr::null_mut()),
            lpszClassName: class_name,
            lpfnWndProc: Some(window_proc),
            ..Default::default()
        };
        if RegisterClassW(&window_class) == 0 {
            return Err(windows::core::Error::from_thread());
        }

        let scale = GetDpiForSystem() as f32 / 96.0;
        let width = (1024.0 * scale) as i32;
        let height = (680.0 * scale) as i32;
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_name,
            w!("Bamboo Music Player"),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            width,
            height,
            None,
            None,
            Some(hinstance),
            None,
        )?;
        info!("main window created");
        DragAcceptFiles(hwnd, true);

        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        let dpi = GetDpiForWindow(hwnd);
        let dpi = if dpi > 0 { dpi as f32 } else { 96.0 };
        let wake = {
            // `HWND` wraps a raw pointer and is not `Send + Sync`; the main
            // window outlives the audio/tray threads, so passing the handle
            // value is safe and keeps the callback usable everywhere.
            let hwnd_raw = hwnd.0 as isize;
            std::sync::Arc::new(move || {
                let window = HWND(hwnd_raw as *mut _);
                let _ = PostMessageW(Some(window), WM_APP_WAKE, WPARAM(0), LPARAM(0));
            }) as Arc<dyn Fn() + Send + Sync>
        };
        // Bench/self-test must not index the user's library or touch its
        // database; normal runs start the library services.
        let load_library = mode.bench.is_none() && !mode.self_test;
        let mut model = App::new(
            crate::core::config::Paths::discover(),
            Some(wake.clone()),
            load_library,
        );
        // Accessibility zoom multiplies the effective DPI: layout, fonts,
        // native controls and mouse coordinates all use it like a real DPI.
        model.set_metrics(
            (client.right - client.left).max(1) as u32,
            (client.bottom - client.top).max(1) as u32,
            dpi,
        );
        grow_to_min_client(
            hwnd,
            (client.right - client.left).max(1),
            (client.bottom - client.top).max(1),
            dpi,
        );
        if let Some(folder) = mode.library.clone() {
            model.core.handle(crate::core::app::Command::LibraryAddFolder(folder));
        }
        if mode.bench.is_some() {
            model.enable_demo();
        } else {
            // Files/directories passed on the command line (open with, jump
            // list) are added before the first paint. Values belonging to
            // switches (`--page folders`, `--folder <dir>`, …) must not be
            // mistaken for files to enqueue.
            let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
            let mut skip = vec![false; args.len()];
            for (index, arg) in args.iter().enumerate() {
                let text = arg.to_string_lossy();
                if text.starts_with("--") && !text.contains('=') {
                    if let Some(next) = args.get(index + 1)
                        && !next.to_string_lossy().starts_with("--")
                    {
                        skip[index + 1] = true;
                    }
                }
            }
            let startup_paths: Vec<std::path::PathBuf> = args
                .iter()
                .enumerate()
                .filter(|(index, _)| !skip[*index])
                .map(|(_, arg)| arg)
                .filter(|arg| !arg.to_string_lossy().starts_with("--"))
                .map(std::path::PathBuf::from)
                .filter(|path| path.exists())
                .collect();
            if !startup_paths.is_empty() {
                model.core.handle(crate::core::app::Command::AddPaths(startup_paths));
                model.refresh_queue_view();
            }
        }
        // Page/theme switches are test and command-line conveniences; apply
        // them after the demo fixture so `--bench --page settings` works.
        // They must not overwrite the user's remembered page on disk.
        let session_page = model.core.session.page;
        if let Some(page) = mode.page.as_deref() {
            let page = match page {
                "queue" => crate::app::Page::Queue,
                "library" => crate::app::Page::Library,
                "folders" => crate::app::Page::Folders,
                "search" => crate::app::Page::Search,
                "discover" | "radio" => crate::app::Page::Discover,
                "stats" | "statistics" => crate::app::Page::Statistics,
                "smart" => crate::app::Page::Smart,
                "duplicates" | "dupes" => crate::app::Page::Duplicates,
                "downloads" | "download" => crate::app::Page::Downloads,
                "settings" => crate::app::Page::Settings,
                _ => crate::app::Page::NowPlaying,
            };
            model.set_page(page);
        }
        if let Some(tab) = mode.settings_tab {
            model.settings_tab =
                crate::app::SettingsTab::ALL[tab.min(crate::app::SettingsTab::ALL.len() - 1)];
        }
        if let Some(theme) = mode.theme {
            model.core.settings.theme.mode = theme;
        }
        if let Some(folder) = mode.folder.clone().filter(|path| path.is_dir()) {
            model.set_page(crate::app::Page::Folders);
            model
                .core
                .handle(crate::core::app::Command::BrowseTo(Some(folder)));
        }
        if mode.page.is_some() || mode.folder.is_some() {
            model.core.session.page = session_page;
        }
        // Native search box: a bare EDIT with a self-drawn frame. IME,
        // selection and clipboard stay with the system control.
        let edit_font = create_edit_font(dpi);
        let palette = model.palette();
        let edit_brush = CreateSolidBrush(crate::paint::edit_background(&palette));
        let list_brush = CreateSolidBrush(crate::paint::list_background(&palette));
        let edit = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("EDIT"),
            w!(""),
            WS_CHILD | WS_VISIBLE | WINDOW_STYLE(0x0080), // ES_AUTOHSCROLL
            0,
            0,
            10,
            10,
            Some(hwnd),
            Some(HMENU(ID_SEARCH as *mut _)),
            Some(hinstance),
            None,
        )?;
        let edit_extensions = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("EDIT"),
            w!(""),
            WS_CHILD | WS_VISIBLE | WINDOW_STYLE(0x0080),
            0,
            0,
            10,
            10,
            Some(hwnd),
            Some(HMENU(ID_EXTENSIONS as *mut _)),
            Some(hinstance),
            None,
        )?;
        let edit_number = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("EDIT"),
            w!(""),
            WS_CHILD | WINDOW_STYLE(0x0001), // ES_CENTER, hidden until editing
            0,
            0,
            10,
            10,
            Some(hwnd),
            Some(HMENU(ID_SETTING_NUMBER as *mut _)),
            Some(hinstance),
            None,
        )?;
        let edit_text_row = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("EDIT"),
            w!(""),
            WS_CHILD | WINDOW_STYLE(0x0080), // ES_AUTOHSCROLL, hidden until editing
            0,
            0,
            10,
            10,
            Some(hwnd),
            Some(HMENU(ID_SETTING_TEXT as *mut _)),
            Some(hinstance),
            None,
        )?;
        let edit_folder = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("EDIT"),
            w!(""),
            WS_CHILD | WS_VISIBLE | WINDOW_STYLE(0x0080), // ES_AUTOHSCROLL
            0,
            0,
            10,
            10,
            Some(hwnd),
            Some(HMENU(ID_FOLDER_PATH as *mut _)),
            Some(hinstance),
            None,
        )?;
        // MFC-style child control: the sort selector is its own LISTBOX, not a
        // painted overlay, so it handles hover/selection/erase itself.
        let sort_list = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("LISTBOX"),
            w!(""),
            WS_CHILD
                | WINDOW_STYLE(0x0001) // LBS_NOTIFY
                | WINDOW_STYLE(0x0020_0000) // WS_VSCROLL
                | WINDOW_STYLE(0x0010) // LBS_OWNERDRAWFIXED
                | WINDOW_STYLE(0x0040) // LBS_HASSTRINGS
                | WINDOW_STYLE(0x0100), // LBS_NOINTEGRALHEIGHT
            0,
            0,
            10,
            10,
            Some(hwnd),
            Some(HMENU(ID_SORT_LIST as *mut _)),
            Some(hinstance),
            None,
        )?;
        {
            let font = edit_font;
            for index in 0..crate::app::LIBRARY_SORTS.len() {
                let label: Vec<u16> = crate::app::App::sort_label_at(index)
                    .encode_utf16()
                    .chain([0])
                    .collect();
                SendMessageW(
                    sort_list,
                    LB_ADDSTRING,
                    Some(WPARAM(0)),
                    Some(LPARAM(label.as_ptr() as isize)),
                );
            }
            let item_height = (crate::layout::SORT_ITEM_HEIGHT * dpi / 96.0).round() as isize;
            SendMessageW(
                sort_list,
                WM_SETFONT,
                Some(WPARAM(font.0 as usize)),
                Some(LPARAM(1)),
            );
            SendMessageW(
                sort_list,
                LB_SETITEMHEIGHT,
                Some(WPARAM(0)),
                Some(LPARAM(item_height)),
            );
            // Dark scrollbar on Windows 10/11; harmless if the theme is absent.
            let _ = SetWindowTheme(
                sort_list,
                w!("DarkMode_Explorer"),
                windows::core::PCWSTR::null(),
            );
            let original = SetWindowLongPtrW(sort_list, GWLP_WNDPROC, sort_list_proc as *const () as isize);
            SORT_ORIG_PROC.store(original, Ordering::Relaxed);
        }
        {
            SendMessageW(
                edit,
                WM_SETFONT,
                Some(WPARAM(edit_font.0 as usize)),
                Some(LPARAM(1)),
            );
            SendMessageW(
                edit_extensions,
                WM_SETFONT,
                Some(WPARAM(edit_font.0 as usize)),
                Some(LPARAM(1)),
            );
            SendMessageW(
                edit_number,
                WM_SETFONT,
                Some(WPARAM(edit_font.0 as usize)),
                Some(LPARAM(1)),
            );
            SendMessageW(
                edit_text_row,
                WM_SETFONT,
                Some(WPARAM(edit_font.0 as usize)),
                Some(LPARAM(1)),
            );
            SendMessageW(
                edit_folder,
                WM_SETFONT,
                Some(WPARAM(edit_font.0 as usize)),
                Some(LPARAM(1)),
            );
            SendMessageW(
                edit_text_row,
                WM_SETFONT,
                Some(WPARAM(edit_font.0 as usize)),
                Some(LPARAM(1)),
            );
            // EM_LIMITTEXT
            SendMessageW(edit, 0x00C5, Some(WPARAM(200)), Some(LPARAM(0)));
            SendMessageW(
                edit_extensions,
                0x00C5,
                Some(WPARAM(400)),
                Some(LPARAM(0)),
            );
            SendMessageW(edit_number, 0x00C5, Some(WPARAM(8)), Some(LPARAM(0)));
            SendMessageW(edit_text_row, 0x00C5, Some(WPARAM(2048)), Some(LPARAM(0)));
            SendMessageW(edit_folder, 0x00C5, Some(WPARAM(1024)), Some(LPARAM(0)));
            set_edit_margins(edit, dpi);
            set_edit_margins(edit_extensions, dpi);
            set_edit_margins(edit_number, dpi);
            set_edit_margins(edit_text_row, dpi);
            set_edit_margins(edit_folder, dpi);
            let original = SetWindowLongPtrW(
                edit_number,
                GWLP_WNDPROC,
                number_edit_proc as *const () as isize,
            );
            NUMBER_ORIG_PROC.store(original, Ordering::Relaxed);
            let original = SetWindowLongPtrW(
                edit_text_row,
                GWLP_WNDPROC,
                text_row_proc as *const () as isize,
            );
            TEXT_ROW_ORIG_PROC.store(original, Ordering::Relaxed);
            let original = SetWindowLongPtrW(
                edit,
                GWLP_WNDPROC,
                search_edit_proc as *const () as isize,
            );
            SEARCH_ORIG_PROC.store(original, Ordering::Relaxed);
            let original = SetWindowLongPtrW(
                edit_folder,
                GWLP_WNDPROC,
                folder_path_proc as *const () as isize,
            );
            FOLDER_PATH_ORIG_PROC.store(original, Ordering::Relaxed);
            // EM_SETCUEBANNER (ignored on systems without comctl32 v6).
            SendMessageW(
                edit,
                0x1501,
                Some(WPARAM(1)),
                Some(LPARAM(w!("搜索曲库…").0 as isize)),
            );
            SendMessageW(
                edit_folder,
                0x1501,
                Some(WPARAM(1)),
                Some(LPARAM(w!("输入路径，按 Enter 跳转").0 as isize)),
            );
        }

        let tray_quit = Arc::new(AtomicBool::new(false));
        let tray_lang = model.core.settings.language;
        let tray_shuffle = model.core.snapshot().shuffle;
        let tray_repeat = model.core.snapshot().repeat;
        let tray = match Tray::new(
            tray_lang,
            [0x3D, 0xB4, 0x8A],
            hwnd.0 as isize,
            tray_quit.clone(),
            wake.clone(),
        ) {
            Ok(tray) => Some(tray),
            Err(err) => {
                warn!("tray unavailable: {err}");
                None
            }
        };
        let tooltip = create_row_tooltip(hwnd, hinstance, edit_font, dpi);
        let initial_key = model.current_track().map(Track::dedupe_key);
        let persist = mode.bench.is_none() && !mode.self_test;
        let ipc = if persist {
            ipc::start_server()
        } else {
            ipc::disabled()
        };
        let smtc = if persist && model.core.settings.features.media_controls {
            SmtcController::new(hwnd.0 as isize)
        } else {
            None
        };
        let palette = model.palette();
        let state = Box::new(State {
            app: model,
            painter: Painter::new()?,
            lyrics: LyricsChild::new(hwnd)?,
            lyrics_window: None,
            lyrics_editor: None,
            panel: PlayerPanel::new(hwnd)?,
            tray,
            desktop_overlay: None,
            mini_overlay: None,
            wake,
            smtc,
            smtc_wanted: false,
            smtc_failed: false,
            ipc,
            last_notify_key: initial_key,
            last_jump_key: Some(String::new()),
            jump_list_on: true,
            applied_capture: None,
            applied_autostart: None,
            applied_associations: None,
            applied_playlist_assoc: None,
            shuffle: tray_shuffle,
            repeat: tray_repeat,
            bench: mode.bench,
            painted: false,
            persist: mode.bench.is_none() && !mode.self_test,
            started: Instant::now(),
            edit,
            edit_extensions,
            edit_number,
            edit_text_row,
            edit_folder,
            sort_list,
            tooltip,
            tooltip_row: None,
            tooltip_setting: None,
            tooltip_visible: false,
            drop_target: None,
            edit_font,
            edit_brush,
            list_brush,
            palette,
            custom_colors: default_custom_colors(),
            mini_key: None,
            desktop_key: None,
            last_page: None,
            backdrop: None,
            last_system_sync: Instant::now() - Duration::from_secs(1),
            last_overlay_sync: Instant::now() - Duration::from_secs(1),
            shortcuts_applied: None,
            gamepad: Gamepad::new(),
            gamepad_running: false,
            last_input: Instant::now(),
            last_trim: Instant::now(),
        });
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(state) as isize);
        if let Some(state) = state_mut(hwnd) {
            state.drop_target = drop_target::install(hwnd);
            sync_tray(state);
            sync_edit_boxes(state, hwnd);
        }

        // Restore the remembered placement before the first show so the window
        // never flashes at the default size. Bench/self-test runs keep the
        // defaults and never write the session.
        let persist = state_mut(hwnd)
            .map(|state| state.persist)
            .unwrap_or(false);
        let start_minimized = persist
            && state_mut(hwnd)
                .map(|state| state.app.core.settings.window.start_minimized)
                .unwrap_or(false);
        let start_maximized = persist && !start_minimized && restore_main_window(hwnd);
        // The remembered/initial size may be smaller than 640×460 logical DIP.
        if let Some(state) = state_mut(hwnd) {
            let mut restored = RECT::default();
            let _ = GetClientRect(hwnd, &mut restored);
            grow_to_min_client(
                hwnd,
                (restored.right - restored.left).max(1),
                (restored.bottom - restored.top).max(1),
                state.app.dpi,
            );
        }
        let _ = ShowWindow(
            hwnd,
            if start_minimized {
                // Minimizing must leave the taskbar button usable; hiding the
                // window outright made "start minimized" behave like tray
                // mode with no way back except the tray menu.
                SW_SHOWMINIMIZED
            } else if start_maximized {
                SW_SHOWMAXIMIZED
            } else {
                SW_SHOW
            },
        );
        if !start_minimized {
            let _ = SetForegroundWindow(hwnd);
        }
        if let Some(state) = state_mut(hwnd) {
            sync_edit_boxes(state, hwnd);
        }
        if mode.self_test || mode.bench.is_some() {
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
            let _ = SetForegroundWindow(hwnd);
        }
        let _ = InvalidateRect(Some(hwnd), None, false);
        let _ = UpdateWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
        if let Some(state) = state_mut(hwnd) {
            sync_lyrics(state, hwnd);
            sync_panel(state, hwnd);
            sync_overlays(state, hwnd);
            sync_system(state, hwnd);
        }
        // Reopen the standalone lyrics window exactly as it was left. Bench
        // and self-test runs never touch the user's session.
        if mode.bench.is_none() && !mode.self_test {
            let (open, windowed) = state_mut(hwnd)
                .map(|state| {
                    let session = &state.app.core.session;
                    (session.lyrics_window, session.lyrics_window_windowed)
                })
                .unwrap_or((false, true));
            if open {
                open_lyrics_window(hwnd, windowed);
            }
        }
        if mode.bench.is_none() && !mode.self_test {
            let args: Vec<String> = std::env::args_os()
                .skip(1)
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            apply_startup_flags(hwnd, parse_startup_flags(&args));
        }
        sync_timer(hwnd);

        match mode.bench {
            Some(seconds) => {
                SetTimer(Some(hwnd), QUIT_TIMER, (seconds * 1000.0) as u32, None);
            }
            None if mode.self_test => {
                SetTimer(Some(hwnd), QUIT_TIMER, 900, None);
            }
            None => {}
        }

        let mut message = MSG::default();
        loop {
            let result = GetMessageW(&mut message, None, 0, 0);
            if result.0 == 0 || result.0 == -1 {
                break;
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }

        let state = take_state(hwnd);
        let _ = UnregisterClassW(class_name, Some(hinstance));
        OleUninitialize();
        CoUninitialize();

        let ticks = state.as_ref().map(|state| state.app.ticks).unwrap_or(0);
        let (frames, avg_paint_us, painted, bench) = match &state {
            Some(state) => {
                let frames = state.app.frames;
                let avg = if frames > 0 {
                    (state.app.paint_nanos / frames as u128 / 1000) as u64
                } else {
                    0
                };
                (frames, avg, state.painted, state.bench)
            }
            None => (0, 0, false, None),
        };
        let (private_mb, ws_mb) = mem_stats();
        let wall = state
            .as_ref()
            .map(|state| state.started.elapsed().as_secs_f32())
            .unwrap_or(0.0);

        if let Some(seconds) = bench {
            println!(
                "bench {seconds:.1}s (wall {wall:.1}s): ticks={ticks}, frames={frames}, avg_paint={avg_paint_us}us, private={private_mb:.1}MB ws={ws_mb:.1}MB"
            );
        }
        if mode.self_test {
            println!("bamboo-music-player self-test: painted={painted}, frames={frames}");
        }

        if mode.self_test && !painted {
            Ok(1)
        } else {
            Ok(0)
        }
    }
}

unsafe fn take_state(hwnd: HWND) -> Option<Box<State>> {
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut State;
    if pointer.is_null() {
        return None;
    }
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        Some(Box::from_raw(pointer))
    }
}

unsafe fn state_mut<'a>(hwnd: HWND) -> Option<&'a mut State> {
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut State;
    unsafe { pointer.as_mut() }
}

fn create_edit_font(dpi: f32) -> HFONT {
    let scale = dpi / 96.0;
    // 13 DIP matches the body text. The previous 13.5 DIP left no room for
    // descenders: the edit's client was only as tall as the character cell,
    // so the tails of g/j/p/q/y were clipped at the bottom edge.
    let height = -(13.0 * scale).round() as i32;
    let face = w!("Microsoft YaHei UI");
    unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            FONT_CHARSET(1),          // DEFAULT_CHARSET
            FONT_OUTPUT_PRECISION(0), // OUT_DEFAULT_PRECIS
            FONT_CLIP_PRECISION(0),   // CLIP_DEFAULT_PRECIS
            FONT_QUALITY(5),          // CLEARTYPE_QUALITY
            0,
            face,
        )
    }
}

fn edit_text(hwnd: HWND) -> String {
    let mut buffer = vec![0u16; 256];
    let written = unsafe { GetWindowTextW(hwnd, &mut buffer) };
    let written = written.max(0) as usize;
    String::from_utf16_lossy(&buffer[..written.min(buffer.len())])
}

/// Line height the edit's current font really needs, measured in the edit's
/// own DC. Guessing from the DIP size clipped descenders whenever the DC and
/// the DPI used for the font disagreed (a DPI-unaware process scales fonts
/// again when the edit paints).
fn edit_line_height(edit: HWND, fallback: i32) -> i32 {
    unsafe {
        let dc = GetDC(Some(edit));
        if dc.is_invalid() {
            return fallback;
        }
        // WM_GETFONT
        let font = SendMessageW(edit, 0x0031, Some(WPARAM(0)), Some(LPARAM(0)));
        let old = if font.0 == 0 {
            HGDIOBJ::default()
        } else {
            SelectObject(dc, HGDIOBJ(font.0 as *mut std::ffi::c_void))
        };
        let mut metrics = TEXTMETRICW::default();
        let measured = GetTextMetricsW(dc, &mut metrics).as_bool();
        if !old.0.is_null() {
            SelectObject(dc, old);
        }
        let _ = ReleaseDC(Some(edit), dc);
        if measured {
            (metrics.tmHeight + metrics.tmExternalLeading).max(1)
        } else {
            fallback
        }
    }
}

fn place_edit(edit: HWND, area: crate::layout::Rect, scale: f32) {
    // A single-line EDIT draws its text at the top of its client area. Size
    // the control to the measured line height plus a small padding, then
    // centre that box inside the painted frame so descenders have room.
    let x = ((area.left + 1.0) * scale).round() as i32;
    let width = (((area.width() - 2.0) * scale).round() as i32).max(10);
    let frame_top = (area.top * scale).round() as i32;
    let frame_height = (area.height() * scale).round() as i32;
    let estimated = (13.0 * scale).round() as i32;
    let line = edit_line_height(edit, estimated);
    // Keep clear of the frame's rounded border: the child window is painted
    // on top of the parent, so an edit that reaches the frame edge hides the
    // bottom border line entirely.
    let height = (line + 6).min((frame_height - 8).max(10));
    // A single-line EDIT paints its text at the top of its client area, so
    // place the *text block* at the frame's vertical centre instead of
    // centring the control (that left the text visibly too high).
    let text_top = (frame_height - line).max(0) / 2;
    let desired = frame_top + (text_top - 1).max(1);
    let max_top = frame_top + (frame_height - 4 - height).max(0);
    let y = desired
        .min(max_top)
        .max(frame_top + 4)
        .min(frame_top + (frame_height - height).max(0));
    let _ = unsafe { MoveWindow(edit, x, y, width, height.max(10), true) };
}

fn place_window(hwnd: HWND, area: crate::layout::Rect, scale: f32) {
    let x = (area.left * scale).round() as i32;
    let y = (area.top * scale).round() as i32;
    let width = ((area.width() * scale).round() as i32).max(10);
    let height = ((area.height() * scale).round() as i32).max(10);
    let _ = unsafe { MoveWindow(hwnd, x, y, width, height, true) };
}

fn set_edit_margins(edit: HWND, dpi: f32) {
    // EM_SETMARGINS: 8 DIP of left/right text padding.
    let margin = (8.0 * dpi / 96.0).round().max(2.0) as u32;
    let value = margin | (margin << 16);
    let _ = unsafe {
        SendMessageW(
            edit,
            0x00D3,
            Some(WPARAM(3)), // EC_LEFTMARGIN | EC_RIGHTMARGIN
            Some(LPARAM(value as isize)),
        )
    };
}

/// Repaint one region immediately. Used when a child window hides: the parent
/// must clear the pixels the child covered before the next timer tick.
fn repaint_region_now(hwnd: HWND, area: crate::layout::Rect, scale: f32) {
    let rect = RECT {
        left: (area.left * scale).floor() as i32,
        top: (area.top * scale).floor() as i32,
        right: (area.right * scale).ceil() as i32,
        bottom: (area.bottom * scale).ceil() as i32,
    };
    let _ = unsafe { InvalidateRect(Some(hwnd), Some(&rect), false) };
    let _ = unsafe { UpdateWindow(hwnd) };
}

fn track_mouse_leave(hwnd: HWND) {
    let mut event = TRACKMOUSEEVENT {
        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE,
        hwndTrack: hwnd,
        dwHoverTime: 0,
    };
    let _ = unsafe { TrackMouseEvent(&mut event) };
}

fn set_edit_text(edit: HWND, text: &str) {
    let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let _ = unsafe { SetWindowTextW(edit, windows::core::PCWSTR(wide.as_ptr())) };
}

/// Sixteen custom swatches remembered by the native color picker.
fn default_custom_colors() -> [COLORREF; 16] {
    [
        COLORREF(0x00FF_FFFF),
        COLORREF(0x00C0_C0C0),
        COLORREF(0x0080_8080),
        COLORREF(0x0040_4040),
        COLORREF(0x0000_0000),
        COLORREF(0x008F_E0B5),
        COLORREF(0x004C_5AFF),
        COLORREF(0x00A7_8BFA),
        COLORREF(0x00FB_9278),
        COLORREF(0x00F5_BE5A),
        COLORREF(0x0078_C8D2),
        COLORREF(0x0059_6EFF),
        COLORREF(0x00FF_6B6B),
        COLORREF(0x006B_FF95),
        COLORREF(0x00FF_D9_5C),
        COLORREF(0x00B0_6BFF),
    ]
}

/// Native `ChooseColorW` dialog. Returns the chosen RGB, or `None` when the
/// user cancels. `custom` is the 16-slot custom palette (BGR COLORREF).
fn choose_color(hwnd: HWND, initial: [u8; 3], custom: &mut [COLORREF; 16]) -> Option<[u8; 3]> {
    let mut dialog = CHOOSECOLORW {
        lStructSize: std::mem::size_of::<CHOOSECOLORW>() as u32,
        hwndOwner: hwnd,
        rgbResult: COLORREF(
            ((initial[2] as u32) << 16) | ((initial[1] as u32) << 8) | initial[0] as u32,
        ),
        lpCustColors: custom.as_mut_ptr(),
        Flags: CC_RGBINIT | CC_FULLOPEN | CC_ANYCOLOR,
        ..Default::default()
    };
    if unsafe { ChooseColorW(&mut dialog) }.as_bool() {
        let value = dialog.rgbResult.0;
        Some([
            (value & 0xff) as u8,
            ((value >> 8) & 0xff) as u8,
            ((value >> 16) & 0xff) as u8,
        ])
    } else {
        None
    }
}

/// Native `ChooseFontW` dialog. Returns the chosen face name, or `None` when
/// cancelled.
fn choose_font(hwnd: HWND, current: Option<&str>) -> Option<String> {
    let mut logfont = LOGFONTW::default();
    if let Some(name) = current {
        for (slot, unit) in logfont
            .lfFaceName
            .iter_mut()
            .zip(name.encode_utf16().take(31))
        {
            *slot = unit;
        }
    }
    let mut dialog = CHOOSEFONTW {
        lStructSize: std::mem::size_of::<CHOOSEFONTW>() as u32,
        hwndOwner: hwnd,
        lpLogFont: &mut logfont,
        Flags: CF_SCREENFONTS | CF_INITTOLOGFONTSTRUCT | CF_FORCEFONTEXIST | CF_NOVERTFONTS,
        ..Default::default()
    };
    if !unsafe { ChooseFontW(&mut dialog) }.as_bool() {
        return None;
    }
    let len = logfont
        .lfFaceName
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(logfont.lfFaceName.len());
    Some(String::from_utf16_lossy(&logfont.lfFaceName[..len]))
}

/// Recreate the native EDIT/LISTBOX brushes when the theme changes, and ask
/// the control to repaint so the new background is visible immediately.
fn sync_native_brushes(state: &mut State) {
    let palette = state.app.palette();
    if palette == state.palette {
        return;
    }
    state.palette = palette;
    unsafe {
        let _ = DeleteObject(HGDIOBJ(state.edit_brush.0));
        let _ = DeleteObject(HGDIOBJ(state.list_brush.0));
    }
    state.edit_brush = unsafe { CreateSolidBrush(crate::paint::edit_background(&palette)) };
    state.list_brush = unsafe { CreateSolidBrush(crate::paint::list_background(&palette)) };
    unsafe {
        let _ = InvalidateRect(Some(state.edit), None, true);
        let _ = InvalidateRect(Some(state.edit_extensions), None, true);
        let _ = InvalidateRect(Some(state.edit_number), None, true);
        let _ = InvalidateRect(Some(state.edit_text_row), None, true);
        let _ = InvalidateRect(Some(state.edit_folder), None, true);
        let _ = InvalidateRect(Some(state.sort_list), None, true);
    }
}

/// Show/position the numeric EDIT over a font-size step value, or hide it.
fn sync_number_edit(state: &mut State, hwnd: HWND) {
    let scale = state.app.dpi / 96.0;
    let active = state.app.page == crate::app::Page::Settings
        && state.app.settings_tab != crate::app::SettingsTab::Library;
    let was_visible = unsafe { IsWindowVisible(state.edit_number) }.as_bool();
    if !active {
        // Switching page/tab changes the row -> field mapping; never commit.
        if was_visible {
            let _ = unsafe { ShowWindow(state.edit_number, SW_HIDE) };
            let _ = unsafe { SetFocus(Some(hwnd)) };
        }
        state.app.number_edit_row = None;
        return;
    }
    let Some(area) = state
        .app
        .number_edit_row
        .and_then(|index| state.app.number_edit_value_rect(index))
    else {
        // The row scrolled out of view: commit what the user typed.
        if was_visible {
            let text = edit_text(state.edit_number);
            state.app.commit_number_edit(hwnd, &text);
            let _ = unsafe { ShowWindow(state.edit_number, SW_HIDE) };
            let _ = unsafe { SetFocus(Some(hwnd)) };
        }
        state.app.number_edit_row = None;
        return;
    };
    place_edit(state.edit_number, area.inset(1.0, 1.0), scale);
    let _ = unsafe { ShowWindow(state.edit_number, SW_SHOW) };
}

fn sync_text_edit(state: &mut State, hwnd: HWND) {
    let scale = state.app.dpi / 96.0;
    let active = state.app.page == crate::app::Page::Settings
        && state.app.settings_tab != crate::app::SettingsTab::Library;
    let was_visible = unsafe { IsWindowVisible(state.edit_text_row) }.as_bool();
    if !active {
        if was_visible {
            let _ = unsafe { ShowWindow(state.edit_text_row, SW_HIDE) };
            let _ = unsafe { SetFocus(Some(hwnd)) };
        }
        state.app.text_edit_row = None;
        return;
    }
    let Some(area) = state
        .app
        .text_edit_row
        .and_then(|index| state.app.text_edit_value_rect(index))
    else {
        if was_visible {
            let text = edit_text(state.edit_text_row);
            state.app.commit_text_edit(hwnd, &text);
            let _ = unsafe { ShowWindow(state.edit_text_row, SW_HIDE) };
            let _ = unsafe { SetFocus(Some(hwnd)) };
        }
        state.app.text_edit_row = None;
        return;
    };
    place_edit(state.edit_text_row, area.inset(1.0, 1.0), scale);
    let _ = unsafe { ShowWindow(state.edit_text_row, SW_SHOW) };
}

fn restore_window(state: &mut State, hwnd: HWND) {
    state.app.restore_visibility(hwnd);
    state.lyrics.mark_dirty();
    sync_panel(state, hwnd);
    sync_lyrics(state, hwnd);
    sync_edit_boxes(state, hwnd);
    sync_inline_edits(state, hwnd);
    unsafe { sync_timer(hwnd) };
}

fn sync_inline_edits(state: &mut State, hwnd: HWND) {
    sync_number_edit(state, hwnd);
    sync_text_edit(state, hwnd);
}

/// Show/position the two native EDITs for the current page. Only the library
/// and settings pages use them; elsewhere they hide so keyboard focus returns
/// to the main window.
fn sync_edit_boxes(state: &mut State, hwnd: HWND) {
    sync_native_brushes(state);
    let scale = state.app.dpi / 96.0;
    let page = state.app.page;
    let page_changed = state.last_page != Some(page);

    if page == crate::app::Page::Library || page == crate::app::Page::Search {
        let area = if page == crate::app::Page::Search {
            state.app.layout.online_search
        } else {
            state.app.layout.search
        };
        place_edit(state.edit, area, scale);
        let _ = unsafe { ShowWindow(state.edit, SW_SHOW) };
        if page_changed {
            // The edit frame is painted by the parent around the child. A
            // stale exposed-rect cache can leave that thin strip unpainted
            // after a page switch; repaint it unconditionally.
            state.app.invalidate_force(hwnd, area.inflate(6.0));
            let (text, cue) = if page == crate::app::Page::Search {
                (
                    state.app.core.snapshot().online.query.clone(),
                    w!("搜索在线曲库，或粘贴链接后回车…"),
                )
            } else {
                (
                    state.app.core.library_search_text().to_string(),
                    w!("搜索曲库…"),
                )
            };
            set_edit_text(state.edit, &text);
            unsafe {
                SendMessageW(
                    state.edit,
                    0x1501,
                    Some(WPARAM(1)),
                    Some(LPARAM(cue.0 as isize)),
                );
            }
        }
    } else if page == crate::app::Page::Queue {
        place_edit(state.edit, state.app.layout.queue_filter, scale);
        let _ = unsafe { ShowWindow(state.edit, SW_SHOW) };
        if page_changed {
            state
                .app
                .invalidate_force(hwnd, state.app.layout.queue_filter.inflate(6.0));
            let text = state.app.queue_filter_text().to_string();
            set_edit_text(state.edit, &text);
            unsafe {
                SendMessageW(
                    state.edit,
                    0x1501,
                    Some(WPARAM(1)),
                    Some(LPARAM(w!("筛选队列…").0 as isize)),
                );
            }
        }
    } else {
        let _ = unsafe { ShowWindow(state.edit, SW_HIDE) };
    }

    // Folder address bar: always visible on the folder page. Do not overwrite
    // what the user is typing; sync the path only when the field is not
    // focused (navigation via toolbar/rows) or the page was just entered.
    if page == crate::app::Page::Folders {
        place_edit(state.edit_folder, state.app.layout.folder_address, scale);
        let _ = unsafe { ShowWindow(state.edit_folder, SW_SHOW) };
        if page_changed {
            state
                .app
                .invalidate_force(hwnd, state.app.layout.folder_address.inflate(6.0));
        }
        let focused = state.app.ui.focused(crate::ui::Input::FolderPath);
        if page_changed || !focused {
            let text = state.app.folder_address_text();
            if edit_text(state.edit_folder) != text {
                set_edit_text(state.edit_folder, &text);
            }
        }
    } else {
        let _ = unsafe { ShowWindow(state.edit_folder, SW_HIDE) };
    }

    if page == crate::app::Page::Settings
        && state.app.settings_tab == crate::app::SettingsTab::Library
    {
        if state.last_page != Some(crate::app::Page::Settings) {
            let text = state.app.settings_extensions_text();
            set_edit_text(state.edit_extensions, &text);
        }
        let area = crate::layout::settings(&state.app.layout).ext_edit;
        let was_visible = unsafe { IsWindowVisible(state.edit_extensions) }.as_bool();
        place_edit(state.edit_extensions, area, scale);
        let _ = unsafe { ShowWindow(state.edit_extensions, SW_SHOW) };
        if !was_visible {
            // Entering this tab shows the edit; repaint its frame outside the
            // occlusion cache (same issue as the queue filter strip).
            state.app.invalidate_force(hwnd, area.inflate(6.0));
        }
    } else {
        let _ = unsafe { ShowWindow(state.edit_extensions, SW_HIDE) };
    }
    // Native sort LISTBOX: only exists on the library page, under the button.
    // Use the actual child visibility: the open flag is already cleared by the
    // click that closes the menu, so it cannot tell us whether pixels remain.
    let was_visible = unsafe { IsWindowVisible(state.sort_list) }.as_bool();
    if page == crate::app::Page::Library {
        let area = crate::layout::sort_menu_rect(&state.app.layout);
        // No inset: the listbox client must fit all items exactly.
        place_window(state.sort_list, area, scale);
        unsafe {
            SendMessageW(
                state.sort_list,
                LB_SETCURSEL,
                Some(WPARAM(state.app.library_sort_index())),
                Some(LPARAM(0)),
            );
            let _ = ShowWindow(
                state.sort_list,
                if state.app.sort_menu_open {
                    SW_SHOW
                } else {
                    SW_HIDE
                },
            );
        }
    } else {
        let _ = unsafe { ShowWindow(state.sort_list, SW_HIDE) };
        SORT_HOVER.store(-1, Ordering::Relaxed);
    }
    // Hiding a child window does not repaint the parent underneath it. Paint
    // that region immediately so the closed list leaves no residue even when
    // the main window is idle (no timer running).
    if was_visible && !unsafe { IsWindowVisible(state.sort_list) }.as_bool() {
        let menu = crate::layout::sort_menu_rect(&state.app.layout).inflate(2.0);
        state.app.invalidate_force(hwnd, menu);
        repaint_region_now(hwnd, menu, scale);
    }
    state.last_page = Some(page);
    sync_inline_edits(state, hwnd);
    sync_lyrics(state, hwnd);
    sync_panel(state, hwnd);
    update_tooltip_hover(hwnd, state);
}

fn sync_panel(state: &mut State, _hwnd: HWND) {
    if state.app.take_player_panel_closed() {
        // The flyout floated over the lyric child; repaint what it covered.
        state.lyrics.mark_dirty();
    }
    state.panel.update(&state.app);
}

/// Sync the embedded lyric panel and, when open, the standalone window. The
/// standalone window ignores the main page and window visibility, so it keeps
/// following playback while the player is hidden or on another display.
fn sync_lyrics(state: &mut State, hwnd: HWND) {
    state.lyrics.update(hwnd, &state.app);
    if let Some(window) = state.lyrics_window.as_mut() {
        window.sync(&state.app);
    }
}

/// Open or close the standalone lyrics window and remember the choice.
fn toggle_lyrics_window(hwnd: HWND) {
    let Some(state) = (unsafe { state_mut(hwnd) }) else {
        return;
    };
    if state.lyrics_window.is_some() {
        close_lyrics_window(hwnd);
    } else {
        // Clicking the button always opens a normal window; F11 switches to
        // borderless fullscreen, and that mode is remembered for restarts.
        open_lyrics_window(hwnd, true);
    }
}

fn open_lyrics_window(hwnd: HWND, windowed: bool) {
    let Some(state) = (unsafe { state_mut(hwnd) }) else {
        return;
    };
    if state.lyrics_window.is_some() {
        return;
    }
    let pos = state.app.core.session.lyrics_window_pos;
    let size = state.app.core.session.lyrics_window_size;
    match LyricsWindow::new(hwnd, pos, size, state.app.dpi, windowed) {
        Ok(window) => {
            state.lyrics_window = Some(window);
            state.app.lyrics_window_open = true;
            state.app
                .core
                .remember_lyrics_window(true, windowed, None, None);
            if let Some(window) = state.lyrics_window.as_mut() {
                window.show(&state.app);
            }
            info!("standalone lyrics window opened (windowed={windowed})");
        }
        Err(err) => warn!("standalone lyrics window unavailable: {err}"),
    }
    state.app.invalidate(hwnd, state.app.layout.lyric_header);
}

fn close_lyrics_window(hwnd: HWND) {
    let Some(state) = (unsafe { state_mut(hwnd) }) else {
        return;
    };
    let Some(window) = state.lyrics_window.take() else {
        return;
    };
    let (pos, size) = window.geometry(state.app.dpi);
    let windowed = window.is_windowed();
    window.hide();
    state.app.lyrics_window_open = false;
    state.app
        .core
        .remember_lyrics_window(false, windowed, pos, size);
    state.app.invalidate(hwnd, state.app.layout.lyric_header);
    info!("standalone lyrics window closed (windowed={windowed})");
}

/// Open (or focus) the native lyrics editor. Must run after the caller's
/// `State` borrow has ended: it builds a fresh `LyricsEditor` from the snapshot
/// and creates a top-level window.
fn open_lyrics_editor(hwnd: HWND) {
    let Some(state) = (unsafe { state_mut(hwnd) }) else {
        return;
    };
    if state.lyrics_editor.is_some() {
        return;
    }
    match LyricsEditorWindow::new(hwnd, &state.app) {
        Ok(editor) => {
            state.lyrics_editor = Some(editor);
            info!("lyrics editor opened");
        }
        Err(err) => warn!("lyrics editor unavailable: {err}"),
    }
}

/// The editor window destroyed itself; drop the handle.
fn lyrics_editor_closed(hwnd: HWND) {
    if let Some(state) = unsafe { state_mut(hwnd) } {
        state.lyrics_editor = None;
    }
}

/// Reload the lyrics file written by the editor. The path pointer is owned by
/// this handler (`Box::into_raw` on the editor side).
fn lyrics_editor_saved(hwnd: HWND, path: *mut std::path::PathBuf) {
    if path.is_null() {
        return;
    }
    let path = unsafe { *Box::from_raw(path) };
    if let Some(state) = unsafe { state_mut(hwnd) } {
        let has_current = state.app.core.snapshot().current.is_some();
        if !has_current {
            return;
        }
        state
            .app
            .core
            .handle(crate::core::app::Command::LoadLyricsFile(path));
        state.app.invalidate_all(hwnd);
        sync_lyrics(state, hwnd);
        sync_panel(state, hwnd);
    }
}

/// The user closed the standalone window with its X button: drop the handle
/// and persist the geometry.
fn lyrics_window_closed(hwnd: HWND) {    let Some(state) = (unsafe { state_mut(hwnd) }) else {
        return;
    };
    let Some(window) = state.lyrics_window.take() else {
        return;
    };
    let (pos, size) = window.geometry(state.app.dpi);
    let windowed = window.is_windowed();
    state.app.lyrics_window_open = false;
    state.app
        .core
        .remember_lyrics_window(false, windowed, pos, size);
    state.app.invalidate(hwnd, state.app.layout.lyric_header);
    info!("standalone lyrics window closed by the user");
}

/// Apply the remembered main-window placement (stored in DIPs). Returns
/// whether the window should open maximized. A position that no longer lands
/// on any monitor is dropped (common after unplugging a display) while the
/// remembered size is kept.
fn restore_main_window(hwnd: HWND) -> bool {
    let Some((pos, size, maximized)) = (unsafe { state_mut(hwnd) }).map(|state| {
        let session = &state.app.core.session;
        (
            session.window_pos,
            session.window_size,
            session.window_maximized,
        )
    }) else {
        return false;
    };
    if pos.is_none() && size.is_none() && !maximized {
        return false;
    }
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let scale = if dpi > 0 { dpi as f32 / 96.0 } else { 1.0 };
    let mut current = RECT::default();
    let _ = unsafe { GetWindowRect(hwnd, &mut current) };
    let width = size
        .map(|(width, _)| (width * scale).round() as i32)
        .unwrap_or(current.right - current.left)
        .max((640.0 * scale) as i32);
    let height = size
        .map(|(_, height)| (height * scale).round() as i32)
        .unwrap_or(current.bottom - current.top)
        .max((460.0 * scale) as i32);
    let placement = pos.map(|(x, y)| ((x * scale).round() as i32, (y * scale).round() as i32));
    if let Some((x, y)) = placement
        && placement_on_screen(x, y, width)
    {
        let _ = unsafe {
            SetWindowPos(
                hwnd,
                None,
                x,
                y,
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            )
        };
    } else {
        let _ = unsafe {
            SetWindowPos(
                hwnd,
                None,
                0,
                0,
                width,
                height,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            )
        };
    }
    maximized
}

/// Require a usable strip of the title bar inside the virtual screen.
fn placement_on_screen(x: i32, y: i32, width: i32) -> bool {
    let left = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let top = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    let right = left + unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
    let bottom = top + unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
    x + width.min(240) > left && x < right && y + 30 > top && y < bottom
}

/// Persist the current main-window placement (outer rect in DIPs). Saved on
/// WM_CLOSE, which also covers "hide to tray" and the tray Quit path.
fn remember_main_window(hwnd: HWND) {
    let Some(state) = (unsafe { state_mut(hwnd) }) else {
        return;
    };
    if !state.persist {
        return;
    }
    let dpi = state.app.dpi;
    let scale = if dpi > 0.0 { dpi / 96.0 } else { 1.0 };
    let mut placement = WINDOWPLACEMENT {
        length: size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    let rect = if unsafe { GetWindowPlacement(hwnd, &mut placement) }.is_ok() {
        // The normal rect is the restored placement even while maximized.
        placement.rcNormalPosition
    } else {
        let mut rect = RECT::default();
        let _ = unsafe { GetWindowRect(hwnd, &mut rect) };
        rect
    };
    let width = (rect.right - rect.left) as f32 / scale;
    let height = (rect.bottom - rect.top) as f32 / scale;
    if width < 200.0 || height < 200.0 {
        return;
    }
    let pos = (rect.left as f32 / scale, rect.top as f32 / scale);
    let maximized =
        unsafe { IsZoomed(hwnd) }.as_bool() || placement.flags.contains(WPF_RESTORETOMAXIMIZED);
    state
        .app
        .core
        .remember_main_window(Some(pos), Some((width, height)), maximized);
}

/// Modal "open audio files" picker shared by the card button and Ctrl+O.
/// Must be called after the caller's `State` borrow has ended.
fn pick_audio_files(hwnd: HWND) {
    let Some(paths) = rfd::FileDialog::new()
        .add_filter(
            crate::i18n::tr("音频"),
            &[
                "mp3", "flac", "wav", "m4a", "aac", "ogg", "opus", "wma", "ape", "wv", "aiff",
                "alac",
            ],
        )
        .pick_files()
    else {
        return;
    };
    if let Some(state) = unsafe { state_mut(hwnd) } {
        let before = state.app.core.snapshot().queue.len();
        state
            .app
            .core
            .handle(crate::core::app::Command::AddPaths(paths));
        let after = state.app.core.snapshot().queue.len();
        state.app.refresh_queue_view();
        state.app.invalidate_all(hwnd);
        if after > before {
            let added = after - before;
            state
                .app
                .push_toast(hwnd, crate::trf!("已加入 {} 首", added), false);
        } else if after == 0 {
            state.app.push_toast(
                hwnd,
                crate::i18n::tr("没有找到可播放的文件"),
                true,
            );
        }
        sync_lyrics(state, hwnd);
        sync_panel(state, hwnd);
        unsafe { sync_timer(hwnd) };
    }
}

fn show_error(hwnd: HWND, title: &str, message: &str) {
    let body: Vec<u16> = message.encode_utf16().chain([0]).collect();
    let caption: Vec<u16> = title.encode_utf16().chain([0]).collect();
    unsafe {
        let _ = MessageBoxW(
            Some(hwnd),
            PCWSTR(body.as_ptr()),
            PCWSTR(caption.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
    if let Some(state) = unsafe { state_mut(hwnd) } {
        state.app.invalidate_after_modal(hwnd);
    }
}

/// Grow the outer window so the client keeps at least 640×460 logical DIP
/// under the current DPI.
fn grow_to_min_client(hwnd: HWND, width_px: i32, height_px: i32, dpi: f32) {
    let scale = dpi / 96.0;
    let min_w = (640.0 * scale).round() as i32;
    let min_h = (460.0 * scale).round() as i32;
    if width_px >= min_w && height_px >= min_h {
        return;
    }
    let mut frame = RECT::default();
    let _ = unsafe { GetWindowRect(hwnd, &mut frame) };
    let target_w = (frame.right - frame.left) + (min_w - width_px).max(0);
    let target_h = (frame.bottom - frame.top) + (min_h - height_px).max(0);
    let _ = unsafe {
        SetWindowPos(
            hwnd,
            None,
            0,
            0,
            target_w,
            target_h,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        )
    };
}

/// Settings import/export/reset. Runs after the click borrow ends so the
/// native dialogs can pump messages.
fn perform_settings_action(hwnd: HWND, action: crate::app::SettingsAction) {
    use crate::app::SettingsAction as Action;
    match action {
        Action::Export => {
            let Some(path) = rfd::FileDialog::new()
                .add_filter(crate::i18n::tr("JSON 设置文件"), &["json"])
                .set_file_name("bamboo-settings.json")
                .save_file()
            else {
                return;
            };
            let result = unsafe { state_mut(hwnd) }
                .map(|state| state.app.core.export_settings(&path));
            if let Some(state) = unsafe { state_mut(hwnd) } {
                match result {
                    Some(Ok(())) => {
                        state.app.push_toast(
                            hwnd,
                            crate::trf!("设置已导出：{}", path.display()),
                            false,
                        );
                    }
                    Some(Err(err)) => state.app.push_toast(
                        hwnd,
                        crate::trf!("导出设置失败：{}", err),
                        true,
                    ),
                    None => {}
                }
                unsafe { sync_timer(hwnd) };
            }
        }
        Action::Import => {
            let Some(path) = rfd::FileDialog::new()
                .add_filter(crate::i18n::tr("JSON 设置文件"), &["json"])
                .pick_file()
            else {
                return;
            };
            let result = unsafe { state_mut(hwnd) }
                .map(|state| state.app.core.import_settings(&path));
            match result {
                Some(Ok(())) => {
                    apply_reloaded_settings(hwnd);
                    if let Some(state) = unsafe { state_mut(hwnd) } {
                        state
                            .app
                            .push_toast(hwnd, crate::i18n::tr("设置已导入"), false);
                        unsafe { sync_timer(hwnd) };
                    }
                }
                Some(Err(err)) => {
                    if let Some(state) = unsafe { state_mut(hwnd) } {
                        state.app.push_toast(
                            hwnd,
                            crate::trf!("导入设置失败：{}", err),
                            true,
                        );
                        unsafe { sync_timer(hwnd) };
                    } else {
                        show_error(hwnd, crate::i18n::tr("导入设置失败"), &err);
                    }
                }
                None => {}
            }
        }
        Action::Reset => {
            let prompt: Vec<u16> = crate::i18n::tr(
                "恢复所有设置为默认值？\r\n这会移除自定义颜色、歌词/overlay 样式和目录设置。",
            )
            .encode_utf16()
            .chain([0])
            .collect();
            let answer = unsafe {
                MessageBoxW(
                    Some(hwnd),
                    PCWSTR(prompt.as_ptr()),
                    w!("Bamboo Music Player"),
                    MB_YESNO | MB_ICONWARNING,
                )
            };
            if answer == IDYES {
                if let Some(state) = unsafe { state_mut(hwnd) } {
                    state.app.core.reset_settings();
                }
                apply_reloaded_settings(hwnd);
                if let Some(state) = unsafe { state_mut(hwnd) } {
                    state
                        .app
                        .push_toast(hwnd, crate::i18n::tr("设置已恢复默认"), false);
                    unsafe { sync_timer(hwnd) };
                }
            }
        }
    }
}

/// Refresh every shell surface after the whole settings tree changed.
fn apply_reloaded_settings(hwnd: HWND) {
    let Some(state) = (unsafe { state_mut(hwnd) }) else {
        return;
    };
    // Fonts/palette are cached by the painter; rebuild them from the new
    // settings before the next paint.
    state.painter.dpi_changed();
    state.app.ui.set_hover(None);
    let mut client = RECT::default();
    let _ = unsafe { GetClientRect(hwnd, &mut client) };
    let width = (client.right - client.left).max(1) as u32;
    let height = (client.bottom - client.top).max(1) as u32;
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let dpi = if dpi > 0 { dpi as f32 } else { 96.0 };
    // Zooming in can shrink the logical client below the 640×460 layout
    // minimum; grow the frame so the layout is not clamped off-screen.
    grow_to_min_client(hwnd, width as i32, height as i32, dpi);
    state.app.set_metrics(width, height, dpi);
    state.painter.resize(width, height, dpi);
    state.app.invalidate_after_modal(hwnd);
    sync_edit_boxes(state, hwnd);
    sync_lyrics(state, hwnd);
    sync_panel(state, hwnd);
    sync_overlays(state, hwnd);
    sync_system(state, hwnd);
    sync_tray(state);
    unsafe { sync_timer(hwnd) };
}

/// Modal playlist picker shared by the queue header, jump list and IPC.
fn pick_playlist(hwnd: HWND) {
    let Some(path) = rfd::FileDialog::new()
        .add_filter("Bamboo Playlist", &["mp3list", "m3u", "m3u8"])
        .pick_file()
    else {
        return;
    };
    if let Some(state) = unsafe { state_mut(hwnd) } {
        state
            .app
            .core
            .handle(crate::core::app::Command::AddPaths(vec![path]));
        state.app.refresh_queue_view();
        state.app.invalidate_all(hwnd);
        let count = state.app.core.snapshot().queue.len();
        state.app.push_toast(
            hwnd,
            if count > 0 {
                crate::trf!("歌单已载入 · {} 首", count)
            } else {
                crate::i18n::tr("歌单为空或无法读取").to_string()
            },
            count == 0,
        );
        sync_lyrics(state, hwnd);
        sync_panel(state, hwnd);
        sync_overlays(state, hwnd);
        sync_system(state, hwnd);
        unsafe { sync_timer(hwnd) };
    }
}

/// Command-line switches created by jump-list tasks and forwarded by a second
/// instance. Files are handled separately (they become the play queue).
#[derive(Clone, Copy, Default)]
struct StartupFlags {
    open_files: bool,
    import_playlist: bool,
    play_pause: bool,
    next: bool,
}

fn parse_startup_flags(args: &[String]) -> StartupFlags {
    let mut flags = StartupFlags::default();
    for arg in args {
        match arg.as_str() {
            "--open-files" => flags.open_files = true,
            "--import-playlist" => flags.import_playlist = true,
            "--play-pause" => flags.play_pause = true,
            "--next" => flags.next = true,
            _ => {}
        }
    }
    flags
}

/// Run non-file startup actions after the window is visible.
fn apply_startup_flags(hwnd: HWND, flags: StartupFlags) {
    if flags.play_pause || flags.next {
        if let Some(state) = unsafe { state_mut(hwnd) } {
            if flags.play_pause {
                state.app.toggle_play(hwnd);
            }
            if flags.next {
                state.app.next(hwnd);
            }
            sync_lyrics(state, hwnd);
            sync_panel(state, hwnd);
            unsafe { sync_timer(hwnd) };
        }
    }
    if flags.open_files {
        pick_audio_files(hwnd);
    }
    if flags.import_playlist {
        pick_playlist(hwnd);
    }
}

/// A second instance / jump-list link forwarded its command line.
fn handle_forwarded_args(hwnd: HWND, args: &[String]) {
    let paths: Vec<std::path::PathBuf> = args
        .iter()
        .filter(|arg| !arg.starts_with("--"))
        .map(std::path::PathBuf::from)
        .filter(|path| path.exists())
        .collect();
    if let Some(state) = unsafe { state_mut(hwnd) } {
        if !paths.is_empty() {
            state
                .app
                .core
                .handle(crate::core::app::Command::AddPaths(paths));
            state.app.refresh_queue_view();
            state.app.invalidate_all(hwnd);
            sync_lyrics(state, hwnd);
            sync_panel(state, hwnd);
            sync_overlays(state, hwnd);
            sync_system(state, hwnd);
        }
    }
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
    apply_startup_flags(hwnd, parse_startup_flags(args));
    unsafe { sync_timer(hwnd) };
}

/// Native tooltip used for queue/library row information. Created once and
/// driven manually (`TTF_TRACK`), so the tooltip can follow virtual rows.
fn create_row_tooltip(hwnd: HWND, instance: HINSTANCE, font: HFONT, dpi: f32) -> HWND {
    let tooltip = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST,
            w!("tooltips_class32"),
            PCWSTR::null(),
            WS_POPUP | WINDOW_STYLE(TTS_ALWAYSTIP | TTS_NOPREFIX),
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            Some(hwnd),
            None,
            Some(instance),
            None,
        )
    };
    let Ok(tooltip) = tooltip else {
        warn!("row tooltip unavailable: {}", windows::core::Error::from_thread());
        return HWND::default();
    };
    let mut tool = row_tool(hwnd, PWSTR::null());
    unsafe {
        SendMessageW(
            tooltip,
            TTM_ADDTOOLW,
            Some(WPARAM(0)),
            Some(LPARAM(&mut tool as *mut _ as isize)),
        );
        // TTM_SETMAXTIPWIDTH: keep multi-line paths readable instead of one
        // very long line.
        SendMessageW(
            tooltip,
            0x0418,
            Some(WPARAM(0)),
            Some(LPARAM((420.0 * dpi / 96.0) as isize)),
        );
        SendMessageW(
            tooltip,
            WM_SETFONT,
            Some(WPARAM(font.0 as usize)),
            Some(LPARAM(1)),
        );
    }
    tooltip
}

fn row_tool(hwnd: HWND, text: PWSTR) -> TTTOOLINFOW {
    TTTOOLINFOW {
        cbSize: size_of::<TTTOOLINFOW>() as u32,
        uFlags: TTF_TRACK | TTF_ABSOLUTE,
        hwnd,
        uId: 1,
        lpszText: text,
        ..Default::default()
    }
}

/// Track the hovered queue/library row. Hover changes hide the old tooltip and
/// restart the delay; the row is not stored anywhere else.
fn update_tooltip_hover(hwnd: HWND, state: &mut State) {
    let row = state.app.hovered_row();
    let setting = state.app.hovered_setting_row();
    if row == state.tooltip_row && setting == state.tooltip_setting {
        return;
    }
    state.tooltip_row = row;
    state.tooltip_setting = setting;
    hide_row_tooltip(hwnd, state);
    unsafe {
        let _ = KillTimer(Some(hwnd), TOOLTIP_TIMER);
        if row.is_some() || setting.is_some() {
            SetTimer(Some(hwnd), TOOLTIP_TIMER, 450, None);
        }
    }
}

fn clear_row_tooltip(hwnd: HWND, state: &mut State) {
    state.tooltip_row = None;
    state.tooltip_setting = None;
    hide_row_tooltip(hwnd, state);
    unsafe {
        let _ = KillTimer(Some(hwnd), TOOLTIP_TIMER);
    }
}

fn hide_row_tooltip(hwnd: HWND, state: &mut State) {
    unsafe {
        let _ = KillTimer(Some(hwnd), TOOLTIP_TIMER);
        let _ = KillTimer(Some(hwnd), TOOLTIP_HIDE_TIMER);
    }
    if state.tooltip.0.is_null() {
        state.tooltip_visible = false;
        return;
    }
    if state.tooltip_visible {
        let mut tool = row_tool(hwnd, PWSTR::null());
        unsafe {
            SendMessageW(
                state.tooltip,
                TTM_TRACKACTIVATE,
                Some(WPARAM(0)),
                Some(LPARAM(&mut tool as *mut _ as isize)),
            );
        }
    }
    state.tooltip_visible = false;
}

fn show_row_tooltip(hwnd: HWND, state: &mut State) {
    if state.tooltip.0.is_null() || !unsafe { IsWindowVisible(hwnd) }.as_bool() {
        return;
    }
    let text = if let Some(row) = state.tooltip_row {
        // A background refresh can re-sort the page under a stationary cursor.
        if state.app.hovered_row() != Some(row) {
            clear_row_tooltip(hwnd, state);
            return;
        }
        let Some(text) = state.app.row_tooltip(row) else {
            return;
        };
        text
    } else if let Some(index) = state.tooltip_setting {
        if state.app.hovered_setting_row() != Some(index) {
            clear_row_tooltip(hwnd, state);
            return;
        }
        let Some(text) = state.app.setting_tooltip(index) else {
            return;
        };
        text
    } else {
        return;
    };
    let mut wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let mut tool = row_tool(hwnd, PWSTR(wide.as_mut_ptr()));
    unsafe {
        SendMessageW(
            state.tooltip,
            TTM_UPDATETIPTEXTW,
            Some(WPARAM(0)),
            Some(LPARAM(&mut tool as *mut _ as isize)),
        );
        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        let offset = (14.0 * state.app.dpi / 96.0).round() as i32;
        let x = point.x + offset;
        let y = point.y + offset * 2;
        let packed = ((y as u32 & 0xFFFF) << 16) | (x as u32 & 0xFFFF);
        SendMessageW(
            state.tooltip,
            TTM_TRACKPOSITION,
            Some(WPARAM(0)),
            Some(LPARAM(packed as isize)),
        );
        SendMessageW(
            state.tooltip,
            TTM_TRACKACTIVATE,
            Some(WPARAM(1)),
            Some(LPARAM(&mut tool as *mut _ as isize)),
        );
    }
    state.tooltip_visible = true;
    unsafe {
        let _ = KillTimer(Some(hwnd), TOOLTIP_HIDE_TIMER);
        SetTimer(Some(hwnd), TOOLTIP_HIDE_TIMER, 5000, None);
    }
}

/// Apply actions posted by the native overlays. Playback actions map to the
/// same `Command`s as the main UI so both surfaces stay in sync.
fn poll_overlay_actions(state: &mut State, hwnd: HWND) {
    let desktop_actions = state
        .desktop_overlay
        .as_ref()
        .map(|host| host.poll())
        .unwrap_or_default();
    let mini_actions = state
        .mini_overlay
        .as_ref()
        .map(|host| host.poll())
        .unwrap_or_default();
    if desktop_actions.is_empty() && mini_actions.is_empty() {
        return;
    }
    let had_desktop = !desktop_actions.is_empty();
    let had_mini = !mini_actions.is_empty();
    let mut show_main = false;
    let mut open_settings = false;
    let mut reset_desktop = false;
    for action in desktop_actions {
        match action {
            OverlayAction::ToggleLock => {
                let locked = state.app.core.settings.desktop_lyrics.locked;
                state
                    .app
                    .core
                    .handle(crate::core::app::Command::DesktopLyricsSetLock(!locked));
            }
            OverlayAction::ToggleClickThrough => {
                let on = state.app.core.settings.desktop_lyrics.click_through;
                state.app.core.handle(
                    crate::core::app::Command::DesktopLyricsSetClickThrough(!on),
                );
            }
            OverlayAction::ToggleAlwaysOnTop => {
                let on = state.app.core.settings.desktop_lyrics.always_on_top;
                state
                    .app
                    .core
                    .handle(crate::core::app::Command::DesktopLyricsSetAlwaysOnTop(!on));
            }
            OverlayAction::Hide => {
                state
                    .app
                    .core
                    .handle(crate::core::app::Command::ToggleDesktopLyrics);
            }
            OverlayAction::ShowMainWindow => show_main = true,
            OverlayAction::OpenSettings => {
                show_main = true;
                open_settings = true;
            }
            OverlayAction::ResetPosition => {
                state
                    .app
                    .core
                    .handle(crate::core::app::Command::DesktopLyricsResetPosition);
                reset_desktop = true;
            }
            OverlayAction::FontDelta { delta } => state
                .app
                .core
                .handle(crate::core::app::Command::DesktopLyricsFontDelta(delta)),
            OverlayAction::SetLines { lines } => state
                .app
                .core
                .handle(crate::core::app::Command::DesktopLyricsSetLines(lines)),
            OverlayAction::SetAlign { align } => state
                .app
                .core
                .handle(crate::core::app::Command::DesktopLyricsSetAlign(align)),
            OverlayAction::ToggleAutoSize => {
                let on = state.app.core.settings.desktop_lyrics.auto_size;
                state
                    .app
                    .core
                    .handle(crate::core::app::Command::DesktopLyricsSetAutoSize(!on));
            }
            OverlayAction::SetAutoSize { enabled } => state
                .app
                .core
                .handle(crate::core::app::Command::DesktopLyricsSetAutoSize(enabled)),
            OverlayAction::SetSize { width, height } => state
                .app
                .core
                .handle(crate::core::app::Command::DesktopLyricsSetSize { width, height }),
            OverlayAction::Geometry {
                x,
                y,
                width,
                height,
                resized,
            } => state.app.core.handle(
                crate::core::app::Command::DesktopLyricsGeometry {
                    x,
                    y,
                    width,
                    height,
                    resized,
                },
            ),
        }
    }
    if reset_desktop {
        if let Some(host) = state.desktop_overlay.as_ref() {
            host.reset_position();
        }
    }
    for action in mini_actions {
        match action {
            MiniAction::PlayPause => state.app.toggle_play(hwnd),
            MiniAction::Prev => state.app.prev(hwnd),
            MiniAction::Next => state.app.next(hwnd),
            MiniAction::ShowMain => show_main = true,
            MiniAction::Hide => state
                .app
                .core
                .handle(crate::core::app::Command::ToggleMiniPlayer),
            MiniAction::ToggleMute => {
                state.app.core.handle(crate::core::app::Command::ToggleMute);
            }
            MiniAction::SetVolume { volume } => {
                state.app.set_volume(volume.clamp(0.0, 1.0));
            }
            MiniAction::Seek { position_ms } => {
                state
                    .app
                    .seek_to_secs(hwnd, position_ms as f64 / 1000.0);
            }
            MiniAction::Geometry {
                x,
                y,
                width,
                height,
                resized,
            } => state
                .app
                .core
                .handle(crate::core::app::Command::MiniPlayerGeometry {
                    x,
                    y,
                    width,
                    height,
                    resized,
                }),
        }
    }
    if had_desktop {
        if let Some(host) = state.desktop_overlay.as_mut() {
            host.force_state();
        }
    }
    if had_mini {
        if let Some(host) = state.mini_overlay.as_mut() {
            host.force_state();
        }
    }
    if show_main {
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
        }
    }
    if open_settings {
        state.app.set_page(crate::app::Page::Settings);
        state.app.invalidate_all(hwnd);
    }
    state.app.invalidate(hwnd, state.app.layout.desktop_lyrics.inflate(2.0));
    state.app.invalidate(hwnd, state.app.layout.mini_player.inflate(2.0));
    state.app.core.flush_settings();
    sync_panel(state, hwnd);
    sync_tray(state);
}

/// Start/stop the overlay threads to match the settings and push the current
/// snapshot. Called from the timer, wake messages and immediately after a
/// toggle click.
fn sync_overlays(state: &mut State, hwnd: HWND) {
    if !state.persist {
        return;
    }
    poll_overlay_actions(state, hwnd);

    let want_desktop = state.app.desktop_lyrics_open();
    if want_desktop && state.desktop_overlay.is_none() {
        match DesktopHost::new(state.wake.clone()) {
            Ok(host) => {
                info!("desktop lyrics overlay started");
                state.desktop_overlay = Some(host);
            }
            Err(err) => {
                warn!("desktop lyrics overlay unavailable: {err}");
                let settings = &mut state.app.core.settings.desktop_lyrics;
                settings.enabled = false;
                settings.visible_at_start = false;
            }
        }
    } else if !want_desktop && state.desktop_overlay.is_some() {
        state.desktop_overlay = None;
        info!("desktop lyrics overlay stopped");
    }

    let want_mini = state.app.mini_player_open();
    if want_mini && state.mini_overlay.is_none() {
        match MiniHost::new(state.wake.clone()) {
            Ok(host) => {
                info!("mini player overlay started");
                state.mini_overlay = Some(host);
            }
            Err(err) => {
                warn!("mini player overlay unavailable: {err}");
                state.app.core.settings.mini.visible = false;
            }
        }
    } else if !want_mini && state.mini_overlay.is_some() {
        state.mini_overlay = None;
        info!("mini player overlay stopped");
    }

    let key = mini_settings_key(&state.app);
    let mini_changed = state.mini_key != Some(key);
    if mini_changed {
        state.mini_key = Some(key);
    }
    let dkey = desktop_settings_key(&state.app);
    let desktop_changed = state.desktop_key.as_ref() != Some(&dkey);
    if desktop_changed {
        state.desktop_key = Some(dkey);
    }
    if let Some(host) = state.desktop_overlay.as_mut() {
        if desktop_changed {
            host.force_state();
        }
        host.sync(&state.app);
    }
    if let Some(host) = state.mini_overlay.as_mut() {
        if mini_changed {
            host.force_state();
        }
        host.sync(&state.app);
    }
}

/// Windows shell integration: media controls, track-change toasts, jump list,
/// capture protection and registry mirrors. All of it is best-effort and
/// skippable through feature toggles.
fn sync_system(state: &mut State, hwnd: HWND) {
    sync_shortcuts(state, hwnd);
    sync_gamepad(state, hwnd);
    sync_backdrop(state, hwnd);
    sync_smtc(state, hwnd);
    sync_notifications(state, hwnd);
    sync_jump_list(state);
    apply_capture_protection(state, hwnd);
    apply_registry_once(state);
}

/// Register global hotkeys from the settings. Registration is idempotent; a
/// combo another process already owns stays in the settings but is reported
/// once so the user can pick another one.
fn sync_shortcuts(state: &mut State, hwnd: HWND) {
    // While a row waits for a combination, release the global keys so typing
    // the new binding cannot trigger the old action.
    if state.app.shortcut_capture_active() {
        if state.shortcuts_applied.is_some() {
            for index in 0..SHORTCUT_COUNT {
                unsafe {
                    let _ = UnregisterHotKey(Some(hwnd), (HOTKEY_BASE + index) as i32);
                }
            }
            state.shortcuts_applied = None;
        }
        return;
    }
    let desired = shortcuts_applied(&state.app);
    if state.shortcuts_applied.as_ref() == Some(&desired) {
        return;
    }
    for index in 0..SHORTCUT_COUNT {
        unsafe {
            let _ = UnregisterHotKey(Some(hwnd), (HOTKEY_BASE + index) as i32);
        }
    }
    state.shortcuts_applied = Some(desired.clone());
    if !state.persist || !desired.enabled {
        return;
    }
    for (index, combo) in desired.combos.iter().enumerate() {
        // An empty binding is intentional: skip registration entirely.
        if combo.trim().is_empty() {
            continue;
        }
        match crate::shortcuts::parse(combo) {
            Some(hotkey) => {
                let modifiers =
                    HOT_KEY_MODIFIERS(hotkey.modifiers | crate::shortcuts::MOD_NOREPEAT);
                if unsafe {
                    RegisterHotKey(
                        Some(hwnd),
                        (HOTKEY_BASE + index) as i32,
                        modifiers,
                        hotkey.vk,
                    )
                }
                .is_err()
                {
                    state.app.push_toast(
                        hwnd,
                        crate::trf!("快捷键 {} 已被其他程序占用", combo),
                        true,
                    );
                }
            }
            None => state.app.push_toast(
                hwnd,
                crate::trf!("快捷键格式无效：{}", combo),
                true,
            ),
        }
    }
}

/// Real Windows 11 Mica on the title bar (and a matching tint in the client,
/// drawn by `paint::draw_gradient`). On older builds the call fails silently
/// and the application gradient remains.
/// Start/stop XInput polling from the settings toggle and device presence.
fn sync_gamepad(state: &mut State, hwnd: HWND) {
    let want = state.persist
        && state.app.core.settings.shortcuts.gamepad
        && state.gamepad.is_connected();
    if want == state.gamepad_running {
        return;
    }
    state.gamepad_running = want;
    unsafe {
        if want {
            let _ = SetTimer(Some(hwnd), GAMEPAD_TIMER, 100, None);
        } else {
            let _ = KillTimer(Some(hwnd), GAMEPAD_TIMER);
        }
    }
}

fn perform_gamepad_action(state: &mut State, hwnd: HWND, action: GamepadAction) {
    match action {
        GamepadAction::PlayPause => state.app.toggle_play(hwnd),
        GamepadAction::Next => state.app.next(hwnd),
        GamepadAction::Prev => state.app.prev(hwnd),
        GamepadAction::VolumeUp => state.app.change_volume(hwnd, 0.05),
        GamepadAction::VolumeDown => state.app.change_volume(hwnd, -0.05),
        GamepadAction::SeekBack => {
            let step = state.app.core.settings.playback.seek_step_secs as f32;
            state.app.seek_by(hwnd, -step);
        }
        GamepadAction::SeekForward => {
            let step = state.app.core.settings.playback.seek_step_secs as f32;
            state.app.seek_by(hwnd, step);
        }
        GamepadAction::ToggleDesktopLyrics => {
            state
                .app
                .core
                .handle(crate::core::app::Command::ToggleDesktopLyrics);
            state.app.invalidate_all(hwnd);
        }
        GamepadAction::ShowHide => {
            if unsafe { IsWindowVisible(hwnd) }.as_bool() {
                unsafe {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
            } else {
                unsafe {
                    let _ = ShowWindow(hwnd, SW_SHOW);
                    let _ = SetForegroundWindow(hwnd);
                }
                restore_window(state, hwnd);
            }
        }
    }
}

fn sync_shutdown_shortcuts(hwnd: HWND) {
    for index in 0..SHORTCUT_COUNT {
        unsafe {
            let _ = UnregisterHotKey(Some(hwnd), (HOTKEY_BASE + index) as i32);
        }
    }
}

fn sync_backdrop(state: &mut State, hwnd: HWND) {
    let dark = crate::theme::resolve_dark(state.app.core.settings.theme.mode);
    if state.backdrop == Some((crate::core::config::BackgroundMode::Solid, dark)) {
        return;
    }
    state.backdrop = Some((crate::core::config::BackgroundMode::Solid, dark));
    let backdrop: i32 = 1; // DWMSBT_NONE: only the title-bar dark mode remains.
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            &backdrop as *const i32 as *const std::ffi::c_void,
            std::mem::size_of::<i32>() as u32,
        );
        let dark_mode = windows::core::BOOL::from(dark);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark_mode as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<windows::core::BOOL>() as u32,
        );
    }
}

fn sync_smtc(state: &mut State, hwnd: HWND) {
    let enabled = state.persist && state.app.core.settings.features.media_controls;
    if enabled != state.smtc_wanted {
        state.smtc_wanted = enabled;
        state.smtc_failed = false;
    }
    if !enabled {
        state.smtc = None;
        return;
    }
    if state.smtc.is_none() && !state.smtc_failed {
        state.smtc = SmtcController::new(hwnd.0 as isize);
        if state.smtc.is_none() {
            state.smtc_failed = true;
            warn!("system media transport controls unavailable");
        }
    }
    let Some(smtc) = state.smtc.as_mut() else {
        return;
    };
    while let Some(event) = smtc.poll() {
        match event {
            SmtcEvent::Play => {
                if !state.app.is_playing() {
                    state.app.toggle_play(hwnd);
                }
            }
            SmtcEvent::Pause => {
                if state.app.is_playing() {
                    state.app.toggle_play(hwnd);
                }
            }
            SmtcEvent::Next => state.app.next(hwnd),
            SmtcEvent::Previous => state.app.prev(hwnd),
            SmtcEvent::Stop => {
                state.app.core.handle(crate::core::app::Command::Stop);
                state.app.invalidate_all(hwnd);
            }
            SmtcEvent::Seek(secs) => state.app.seek_to_secs(hwnd, secs),
        }
    }
    let snapshot = state.app.core.snapshot();
    let (title, artist, album) = snapshot
        .current
        .map(|track| {
            (
                track.display_title(),
                track.display_artist(),
                track.album.clone(),
            )
        })
        .unwrap_or_default();
    smtc.update(&title, &artist, &album, state.app.is_playing(), snapshot.current.is_some());
    let key = snapshot
        .current
        .map(Track::dedupe_key)
        .unwrap_or_default();
    smtc.set_thumbnail(&key, snapshot.cover_rev, snapshot.current_cover_png);
    let duration = snapshot
        .current
        .map(|_| snapshot.duration_secs)
        .filter(|value| *value > 0.0);
    smtc.update_timeline(snapshot.position_secs, duration);
}

fn sync_notifications(state: &mut State, hwnd: HWND) {
    let key = state.app.current_track().map(Track::dedupe_key);
    if key == state.last_notify_key {
        return;
    }
    let first = state.last_notify_key.is_some() && state.last_notify_key != key;
    state.last_notify_key = key;
    if !first
        || !state.persist
        || !state.app.core.settings.features.notifications
        || state.app.current_track().is_none()
    {
        return;
    }
    // A toast for the track the user just clicked in the foreground is noise.
    if unsafe { GetForegroundWindow() } == hwnd {
        return;
    }
    if let Some(track) = state.app.current_track() {
        let _ = notify::show(&track.display_title(), &track.subtitle());
    }
}

fn sync_jump_list(state: &mut State) {
    // Portable profiles do not own the shell's app id; leave the installed
    // player's jump list untouched.
    if state.app.core.paths.portable {
        return;
    }
    let enabled = state.persist && state.app.core.settings.features.jump_list;
    let key = state.app.current_track().map(Track::dedupe_key);
    if enabled == state.jump_list_on && key == state.last_jump_key {
        return;
    }
    state.jump_list_on = enabled;
    state.last_jump_key = key;
    if !enabled {
        let _ = jump_list::clear();
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let _ = jump_list::update(&exe, state.app.core.recent_files());
}

fn apply_capture_protection(state: &mut State, hwnd: HWND) {
    let on = state.app.core.settings.window.capture_protection;
    if state.applied_capture != Some(on) {
        state.applied_capture = Some(on);
        let _ = unsafe {
            SetWindowDisplayAffinity(
                hwnd,
                if on { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE },
            )
        };
    }
    if let Some(window) = state.lyrics_window.as_ref() {
        window
            .apply_capture_protection(state.app.core.settings.window.lyrics_capture_protection);
    }
}

fn apply_registry_once(state: &mut State) {
    // A portable copy must never rewrite the machine's autostart or file
    // associations; those belong to an installed profile.
    if state.app.core.paths.portable {
        return;
    }
    let want_autostart = state.app.core.settings.features.start_with_windows;
    let want_associations = state.app.core.settings.features.file_associations;
    let want_playlist = state.app.core.settings.features.playlist_associations;
    let want = want_autostart;
    if state.applied_autostart != Some(want) {
        let registry_on = registry::autostart_enabled();
        if registry_on != want {
            if let Err(err) = registry::set_autostart(want) {
                warn!("autostart update failed: {err}");
            }
        }
        state.applied_autostart = Some(want);
    }
    let want = want_associations;
    if state.applied_associations != Some(want) {
        let registry_on = registry::associations_enabled();
        if registry_on != want {
            if let Err(err) = registry::set_associations(want) {
                warn!("file association update failed: {err}");
            }
        }
        state.applied_associations = Some(want);
    }
    let want = want_playlist;
    if state.applied_playlist_assoc != Some(want) {
        let registry_on = registry::playlist_association_enabled();
        if registry_on != want {
            if let Err(err) = registry::set_playlist_association(want) {
                warn!("playlist association update failed: {err}");
            }
        }
        state.applied_playlist_assoc = Some(want);
    }
}

fn sync_tray(state: &mut State) {
    let shortcuts = state.app.core.settings.shortcuts.clone();
    let Some(tray) = state.tray.as_mut() else {
        return;
    };
    tray.set_shortcuts(&shortcuts);
    tray.set_playing(state.app.is_playing());
    tray.set_shuffle(state.shuffle);
    tray.set_repeat(state.repeat);
    tray.set_desktop_lyrics(state.app.desktop_lyrics_open());
    tray.set_locked(state.app.core.settings.desktop_lyrics.locked);
    let tooltip = match state.app.current_track() {
        Some(track) => format!("{}\n{}", track.title, track.artist),
        None => "Bamboo Music Player".to_string(),
    };
    tray.set_tooltip(&tooltip);
}

unsafe fn sync_timer(hwnd: HWND) {
    let Some(state) = (unsafe { state_mut(hwnd) }) else {
        return;
    };
    let standalone = state
        .lyrics_window
        .as_ref()
        .is_some_and(|window| window.is_visible());
    // Overlays need position ticks even when the main window is hidden; they
    // interpolate locally but must never show a frozen clock while playing.
    let overlay_open = state.desktop_overlay.is_some() || state.mini_overlay.is_some();
    let minimized = unsafe { IsIconic(hwnd) }.as_bool();
    let visible = state.app.window_visible && !minimized;
    // A toast must expire even when playback is paused and no overlay runs.
    let toast = state.app.toast_active();
    // The flow background animates independently of playback.
    let performance = &state.app.core.settings.performance;
    let unfocused = unsafe { GetForegroundWindow() } != hwnd;
    let pause_unfocused =
        performance.pause_animations_unfocused && unfocused && !standalone && !overlay_open;
    let playing = state.app.is_playing();
    let visible_anim = playing && !pause_unfocused && visible;
    // Hidden but still playing: the audio engine wakes the UI on every state
    // change, so a slow heartbeat is enough to keep SMTC and the overlays
    // current. Overlays interpolate their own clock between position sends.
    let overlay_heartbeat = playing && !visible && (overlay_open || standalone);
    let system_heartbeat = playing && !visible && !overlay_open && !standalone;
    let animate = state.bench.is_some()
        || toast
        || visible_anim
        || overlay_heartbeat
        || system_heartbeat;
    if animate {
        let frame = state.app.core.settings.performance.frame_millis() as u32;
        let interval = if state.bench.is_some() || visible_anim {
            frame
        } else if overlay_heartbeat {
            // The overlay needs a new position message every 250 ms; its own
            // render timer smooths the motion in between.
            250
        } else {
            // Toast expiry or a hidden player: a 1 s heartbeat is plenty.
            1000
        };
        unsafe { SetTimer(Some(hwnd), ANIM_TIMER, interval.max(8), None) };
    } else {
        let _ = unsafe { KillTimer(Some(hwnd), ANIM_TIMER) };
    }
    // Idle memory trimming does not repaint; it just moves pages to standby.
    let trim = state.app.core.settings.window.trim_working_set_on_idle
        && !state.app.is_playing();
    if trim {
        unsafe { SetTimer(Some(hwnd), IDLE_TRIM_TIMER, 15_000, None) };
    } else {
        let _ = unsafe { KillTimer(Some(hwnd), IDLE_TRIM_TIMER) };
    }
}

fn cursor_dip(hwnd: HWND, dpi: f32) -> Option<(f32, f32)> {
    let mut point = POINT::default();
    unsafe {
        GetCursorPos(&mut point).ok()?;
        let _ = ScreenToClient(hwnd, &mut point);
    }
    let scale = dpi / 96.0;
    Some((point.x as f32 / scale, point.y as f32 / scale))
}

fn message_point(lparam: LPARAM, dpi: f32) -> (f32, f32) {
    let x = (lparam.0 & 0xFFFF) as i16 as f32;
    let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32;
    let scale = dpi / 96.0;
    (x / scale, y / scale)
}

// ---------------------------------------------------------------------------
// Lyrics context menu (right click on the lyric list)
// ---------------------------------------------------------------------------

const LYRICS_MENU_PLAY: usize = 1;
const LYRICS_MENU_COPY_TEXT: usize = 2;
const LYRICS_MENU_COPY_TIME: usize = 3;
const LYRICS_MENU_RELOAD: usize = 4;
const LYRICS_MENU_IMPORT: usize = 5;
const LYRICS_MENU_EXPORT: usize = 6;
const LYRICS_MENU_OPEN: usize = 7;
const LYRICS_MENU_SHARE: usize = 8;

/// Everything needed to render a share card on a worker thread.
struct ShareJob {
    title: String,
    subtitle: String,
    lines: Vec<crate::util::share_image::ShareLine>,
    highlight: Option<usize>,
    cover: Option<Vec<u8>>,
    accent: [u8; 3],
    name: String,
    dir: Option<std::path::PathBuf>,
}

struct LyricsMenuData {
    /// Primary line plus translation (second line), for "复制文本".
    line_text: String,
    timestamp: Option<String>,
    seek_ms: Option<u64>,
    /// LRC text, default file name, default directory.
    export: Option<(String, String, Option<std::path::PathBuf>)>,
    share: Option<ShareJob>,
    lyrics_path: Option<std::path::PathBuf>,
    local: bool,
}

enum LyricsMenuOutcome {
    Nothing,
    Reload,
    Import,
    Export {
        text: String,
        name: String,
        dir: Option<std::path::PathBuf>,
    },
    Share(ShareJob),
}

/// Snapshot the lyrics data needed by the menu. `line_index` comes from the
/// child control (`WM_APP_LYRIC_MENU` wparam - 1).
fn lyrics_menu_data(app: &App, line_index: Option<usize>) -> Option<LyricsMenuData> {
    let snapshot = app.core.snapshot();
    let track = snapshot.current?;
    let lyrics = snapshot.lyrics;
    let line = lyrics.and_then(|lyrics| line_index.and_then(|index| lyrics.lines.get(index)));

    let mut line_text = line
        .map(|line| line.text.trim().to_string())
        .unwrap_or_default();
    if let Some(translation) = line
        .and_then(|line| line.translation.as_deref())
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        if !line_text.is_empty() {
            line_text.push('\n');
        }
        line_text.push_str(translation);
    }

    let seek_ms = line
        .filter(|_| lyrics.is_some_and(|lyrics| lyrics.timed))
        .map(|line| line.start_ms);
    let timestamp = seek_ms.map(|ms| format!("[{}]", crate::lyrics::editor::format_tag(ms)));
    let lyrics_path = lyrics.and_then(|lyrics| lyrics.path.clone());
    let local = track.source.is_local();
    let audio_path = track.source.path().map(|path| path.to_path_buf());

    let export = lyrics.and_then(|lyrics| {
        if lyrics.lines.is_empty() {
            return None;
        }
        let title = if track.title.trim().is_empty() {
            track.source.kind_label().to_string()
        } else {
            track.title.clone()
        };
        let editor = crate::lyrics::editor::LyricsEditor::from_lyrics(
            Some(lyrics),
            track.dedupe_key(),
            title,
            audio_path.clone(),
        );
        let text = editor.to_lrc();
        if text.trim().is_empty() {
            return None;
        }
        let name = lyrics_path
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .or_else(|| {
                audio_path.as_ref().and_then(|path| {
                    path.file_stem()
                        .map(|stem| format!("{}.lrc", stem.to_string_lossy()))
                })
            })
            .unwrap_or_else(|| "lyrics.lrc".to_string());
        let dir = lyrics_path
            .as_ref()
            .and_then(|path| path.parent())
            .or_else(|| audio_path.as_ref().and_then(|path| path.parent()))
            .map(|path| path.to_path_buf());
        Some((text, name, dir))
    });

    // Share card: five lines centred on the clicked/active line.
    let share = lyrics.and_then(|lyrics| {
        if lyrics.lines.is_empty() {
            return None;
        }
        let active = lyrics.index_at((snapshot.position_secs * 1000.0).max(0.0) as u64);
        let focus = line_index.or(active);
        let start = focus.map(|index| index.saturating_sub(2)).unwrap_or(0);
        let lines: Vec<crate::util::share_image::ShareLine> = lyrics
            .lines
            .iter()
            .skip(start)
            .take(5)
            .map(|line| crate::util::share_image::ShareLine {
                text: line.text.clone(),
                translation: line
                    .translation
                    .as_deref()
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(str::to_string),
            })
            .collect();
        if lines.is_empty() {
            return None;
        }
        let highlight = focus
            .map(|index| index.saturating_sub(start))
            .filter(|index| *index < lines.len());
        let title = track.display_title();
        let subtitle = track.subtitle();
        let name = format!(
            "{}-歌词分享.png",
            sanitize_file_name(&title)
        );
        let dir = lyrics_path
            .as_ref()
            .and_then(|path| path.parent())
            .or_else(|| audio_path.as_ref().and_then(|path| path.parent()))
            .map(|path| path.to_path_buf());
        Some(ShareJob {
            title,
            subtitle,
            lines,
            highlight,
            cover: snapshot.current_cover_png.map(|png| png.to_vec()),
            accent: app.core.settings.theme.accent,
            name,
            dir,
        })
    });

    Some(LyricsMenuData {
        line_text,
        timestamp,
        seek_ms,
        export,
        share,
        lyrics_path,
        local,
    })
}

/// Replace characters Windows refuses in file names.
fn sanitize_file_name(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|ch| match ch {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => ch,
        })
        .collect();
    out = out.trim().trim_end_matches('.').to_string();
    if out.is_empty() {
        "lyrics".to_string()
    } else {
        out
    }
}

/// Show the lyrics menu. Clipboard/seek/location actions run here; reload,
/// import and export are returned to the caller (which may open a dialog).
fn show_lyrics_menu(hwnd: HWND, data: LyricsMenuData) -> LyricsMenuOutcome {
    let mut at = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut at);
    }
    let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
        return LyricsMenuOutcome::Nothing;
    };
    let normal = MF_STRING;
    let disabled = MF_STRING | MF_GRAYED;
    append_menu_item(
        menu,
        if data.seek_ms.is_some() { normal } else { disabled },
        LYRICS_MENU_PLAY,
        crate::i18n::tr("在此处播放"),
    );
    append_menu_item(
        menu,
        if data.line_text.is_empty() { disabled } else { normal },
        LYRICS_MENU_COPY_TEXT,
        crate::i18n::tr("复制文本"),
    );
    append_menu_item(
        menu,
        if data.timestamp.is_some() { normal } else { disabled },
        LYRICS_MENU_COPY_TIME,
        crate::i18n::tr("复制时间点"),
    );
    append_menu_item(menu, MF_SEPARATOR, 0, "");
    append_menu_item(menu, normal, LYRICS_MENU_RELOAD, crate::i18n::tr("重新加载歌词"));
    append_menu_item(menu, normal, LYRICS_MENU_IMPORT, crate::i18n::tr("导入歌词"));
    append_menu_item(
        menu,
        if data.export.is_some() { normal } else { disabled },
        LYRICS_MENU_EXPORT,
        crate::i18n::tr("导出歌词"),
    );
    append_menu_item(
        menu,
        if data.share.is_some() { normal } else { disabled },
        LYRICS_MENU_SHARE,
        crate::i18n::tr("生成分享图…"),
    );
    let open_enabled = data.local
        && data
            .lyrics_path
            .as_ref()
            .is_some_and(|path| path.is_file());
    append_menu_item(
        menu,
        if open_enabled { normal } else { disabled },
        LYRICS_MENU_OPEN,
        crate::i18n::tr("打开歌词位置"),
    );

    let command = unsafe {
        let _ = SetForegroundWindow(hwnd);
        let command = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            at.x,
            at.y,
            None,
            hwnd,
            None,
        )
        .0 as usize;
        let _ = DestroyMenu(menu);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        command
    };

    match command {
        LYRICS_MENU_PLAY => {
            if let Some(ms) = data.seek_ms {
                unsafe {
                    let _ = PostMessageW(
                        Some(hwnd),
                        WM_APP_LYRIC_SEEK,
                        WPARAM(ms as usize),
                        LPARAM(0),
                    );
                }
            }
        }
        LYRICS_MENU_COPY_TEXT => copy_to_clipboard(&data.line_text),
        LYRICS_MENU_COPY_TIME => {
            if let Some(timestamp) = &data.timestamp {
                copy_to_clipboard(timestamp);
            }
        }
        LYRICS_MENU_RELOAD => return LyricsMenuOutcome::Reload,
        LYRICS_MENU_IMPORT => return LyricsMenuOutcome::Import,
        LYRICS_MENU_EXPORT => {
            if let Some((text, name, dir)) = data.export {
                return LyricsMenuOutcome::Export { text, name, dir };
            }
        }
        LYRICS_MENU_SHARE => {
            if let Some(job) = data.share {
                return LyricsMenuOutcome::Share(job);
            }
        }
        LYRICS_MENU_OPEN => {
            if open_enabled && let Some(path) = &data.lyrics_path {
                open_file_location(path);
            }
        }
        _ => {}
    }
    LyricsMenuOutcome::Nothing
}

// ---------------------------------------------------------------------------
// Queue / library row context menus
// ---------------------------------------------------------------------------

const ROW_MENU_PLAY: usize = 1;
const ROW_MENU_REMOVE: usize = 2;
const ROW_MENU_QUEUE_NEXT: usize = 3;
const ROW_MENU_QUEUE_APPEND: usize = 4;
const ROW_MENU_COPY_TITLE: usize = 5;
const ROW_MENU_COPY_ARTIST: usize = 6;
const ROW_MENU_COPY_PATH: usize = 7;
const ROW_MENU_OPEN_LOCATION: usize = 8;
const ROW_MENU_OPEN_URL: usize = 9;
const ROW_MENU_COPY_ID: usize = 10;
const ROW_MENU_CLEAR: usize = 11;
const ROW_MENU_MOVE_UP: usize = 12;
const ROW_MENU_MOVE_DOWN: usize = 13;
const ROW_MENU_MOVE_TOP: usize = 14;
const ROW_MENU_MOVE_BOTTOM: usize = 15;
const ROW_MENU_ADD_LIBRARY: usize = 16;
const ROW_MENU_LIBRARY_REMOVE: usize = 17;
const ROW_MENU_ONLINE_QUEUE: usize = 18;
const ROW_MENU_ONLINE_DOWNLOAD: usize = 19;
const ROW_MENU_ONLINE_ALBUM: usize = 20;
const ROW_MENU_ONLINE_ARTIST: usize = 21;
const ROW_MENU_DUP_KEEP: usize = 22;
const ROW_MENU_DUP_REMOVE_GROUP: usize = 23;
const ROW_MENU_SMART_QUEUE: usize = 24;
const ROW_MENU_SMART_REMOVE: usize = 25;
const ROW_MENU_QUEUE_DOWNLOAD: usize = 26;

struct RowMenuData {
    page: Page,
    queue_row: bool,
    /// Folder page row: the menu opens/plays/imports the entry instead of
    /// touching the queue.
    folder_row: bool,
    folder_is_dir: bool,
    folder_is_playlist: bool,
    index: usize,
    /// Display row (filtered view); reorder actions only run when the filter
    /// is empty, so this is also the queue index in that case.
    display_index: usize,
    queue_rows: usize,
    title: String,
    artist: String,
    file: Option<std::path::PathBuf>,
    url: Option<String>,
    song_id: Option<String>,
    missing: bool,
    /// Duplicate-page group header row (keep/remove acts on the group).
    header: bool,
    album_id: Option<String>,
    artist_id: Option<String>,
    /// Album search result (its `id` is an album id, not a playable song).
    album_result: bool,
    /// Queue row that can be downloaded (online or direct URL).
    queue_download: bool,
}

enum RowMenuOutcome {
    Nothing,
    Play { queue_row: bool, index: usize },
    Remove { index: usize },
    Queue { index: usize, next: bool },
    Move { from: usize, to: usize },
    ClearQueue,
    FolderPlay(usize),
    FolderEnqueue(usize),
    FolderAddLibrary(std::path::PathBuf),
    LibraryRemove(usize),
    OnlinePlay(usize),
    OnlineQueue(usize),
    OnlineDownload(usize),
    DiscoverPlay(usize),
    DiscoverQueue(usize),
    DiscoverDownload(usize),
    QueueDownload(usize),
    OnlineAlbum(String),
    OnlineArtist(String),
    SmartPlay(usize),
    SmartQueue(usize),
    SmartRemove(usize),
    DuplicateKeep(usize),
    DuplicateRemoveGroup(usize),
    DuplicateRemove(usize),
    DuplicatePlay(usize),
}

fn row_menu_data(app: &App, x: f32, y: f32) -> Option<RowMenuData> {
    let display_index = app.list_row_at(x, y)?;
    let snapshot = app.core.snapshot();
    let mut data = RowMenuData {
        page: app.page,
        queue_row: false,
        folder_row: false,
        folder_is_dir: false,
        folder_is_playlist: false,
        index: display_index,
        display_index,
        queue_rows: 0,
        title: String::new(),
        artist: String::new(),
        file: None,
        url: None,
        song_id: None,
        missing: false,
        header: false,
        album_id: None,
        artist_id: None,
        album_result: false,
        queue_download: false,
    };
    match app.page {
        Page::Queue => {
            let index = app.queue_index(display_index)?;
            let track = snapshot.queue.get(index)?;
            data.queue_row = true;
            data.index = index;
            data.queue_rows = if app.queue_filter_text().trim().is_empty()
                && !app.queue_shows_shuffle()
            {
                app.queue_len()
            } else {
                0
            };
            data.title = track.title.clone();
            data.artist = track.artist.clone();
            match &track.source {
                TrackSource::Local { path } => data.file = Some(path.clone()),
                TrackSource::Url { url } | TrackSource::Radio { url } => {
                    data.url = Some(url.clone());
                    data.queue_download = true;
                }
                TrackSource::Bamboo { song_id, .. } => {
                    data.song_id = Some(song_id.clone());
                    data.queue_download = true;
                }
            }
        }
        Page::Library => {
            let entry = snapshot.library.get(display_index)?;
            data.title = entry.title.clone();
            data.artist = entry.artist.clone();
            data.file = Some(entry.path.clone());
            data.missing = entry.missing;
        }
        Page::Smart => {
            let entry = snapshot.library_smart.get(display_index)?;
            data.title = entry.title.clone();
            data.artist = entry.artist.clone();
            data.file = Some(entry.path.clone());
        }
        Page::Duplicates => {
            let entry = snapshot.library_duplicate_rows.get(display_index)?;
            data.header = snapshot
                .library_duplicate_headers
                .get(display_index)
                .copied()
                .unwrap_or(false);
            data.title = entry.title.clone();
            data.artist = entry.artist.clone();
            if !data.header {
                data.file = Some(entry.path.clone());
            }
        }
        Page::Search => {
            let song = snapshot.online.rows().get(display_index)?;
            data.title = song.name.clone();
            data.artist = song.artist.clone();
            data.album_id = song.album_id.clone();
            data.artist_id = song.artist_id.clone();
            data.song_id = Some(song.id.clone());
            if snapshot.online.kind == crate::core::api::SearchKind::Album
                && !snapshot.online.is_detail()
            {
                data.album_result = true;
                data.album_id = Some(song.id.clone());
                if !song.album.trim().is_empty() {
                    data.title = song.album.clone();
                }
            }
        }
        Page::Discover => {
            let song = snapshot.discover.songs.get(display_index)?;
            data.title = song.name.clone();
            data.artist = song.artist.clone();
            data.song_id = Some(song.id.clone());
        }
        Page::Folders => {
            let entry = snapshot.browser.entries.get(display_index)?;
            data.folder_row = true;
            data.folder_is_dir = entry.is_dir;
            data.folder_is_playlist = entry.is_playlist;
            data.title = entry.name.clone();
            data.file = Some(entry.path.clone());
        }
        _ => return None,
    }
    Some(data)
}

fn show_row_menu(hwnd: HWND, data: RowMenuData) -> RowMenuOutcome {
    if data.folder_row {
        return show_folder_row_menu(hwnd, data);
    }
    match data.page {
        Page::Smart => return show_smart_row_menu(hwnd, data),
        Page::Duplicates => return show_duplicate_row_menu(hwnd, data),
        Page::Search => return show_online_row_menu(hwnd, data),
        Page::Discover => return show_discover_row_menu(hwnd, data),
        _ => {}
    }
    let mut at = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut at);
    }
    let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
        return RowMenuOutcome::Nothing;
    };
    let normal = MF_STRING;
    let disabled = MF_STRING | MF_GRAYED;
    let play_flags = if data.missing { disabled } else { normal };
    append_menu_item(menu, play_flags, ROW_MENU_PLAY, crate::i18n::tr("播放"));
    if data.queue_row && data.queue_download {
        append_menu_item(
            menu,
            normal,
            ROW_MENU_QUEUE_DOWNLOAD,
            crate::i18n::tr("下载"),
        );
    }
    if data.queue_row {
        append_menu_item(
            menu,
            normal,
            ROW_MENU_REMOVE,
            crate::i18n::tr("从队列移除"),
        );
        append_menu_item(menu, MF_SEPARATOR, 0, "");
        let can_up = data.queue_rows > 1 && data.display_index > 0;
        let can_down = data.queue_rows > 1
            && data.display_index + 1 < data.queue_rows;
        append_menu_item(
            menu,
            if can_up { normal } else { disabled },
            ROW_MENU_MOVE_UP,
            crate::i18n::tr("上移"),
        );
        append_menu_item(
            menu,
            if can_down { normal } else { disabled },
            ROW_MENU_MOVE_DOWN,
            crate::i18n::tr("下移"),
        );
        append_menu_item(
            menu,
            if can_up { normal } else { disabled },
            ROW_MENU_MOVE_TOP,
            crate::i18n::tr("置顶"),
        );
        append_menu_item(
            menu,
            if can_down { normal } else { disabled },
            ROW_MENU_MOVE_BOTTOM,
            crate::i18n::tr("置底"),
        );
    } else {
        append_menu_item(
            menu,
            play_flags,
            ROW_MENU_QUEUE_NEXT,
            crate::i18n::tr("下一首播放"),
        );
        append_menu_item(
            menu,
            play_flags,
            ROW_MENU_QUEUE_APPEND,
            crate::i18n::tr("添加到播放队列"),
        );
        append_menu_item(menu, MF_SEPARATOR, 0, "");
        append_menu_item(
            menu,
            normal,
            ROW_MENU_LIBRARY_REMOVE,
            crate::i18n::tr("从曲库移除"),
        );
    }
    append_menu_item(menu, MF_SEPARATOR, 0, "");
    append_menu_item(
        menu,
        if data.title.is_empty() { disabled } else { normal },
        ROW_MENU_COPY_TITLE,
        crate::i18n::tr("复制歌名"),
    );
    append_menu_item(
        menu,
        if data.artist.is_empty() { disabled } else { normal },
        ROW_MENU_COPY_ARTIST,
        crate::i18n::tr("复制歌手"),
    );
    append_menu_item(
        menu,
        if data.file.is_some() { normal } else { disabled },
        ROW_MENU_COPY_PATH,
        crate::i18n::tr("复制文件路径"),
    );
    append_menu_item(
        menu,
        if data.file.is_some() { normal } else { disabled },
        ROW_MENU_OPEN_LOCATION,
        crate::i18n::tr("打开文件位置"),
    );
    append_menu_item(
        menu,
        if data.url.is_some() { normal } else { disabled },
        ROW_MENU_OPEN_URL,
        crate::i18n::tr("打开 URL"),
    );
    append_menu_item(
        menu,
        if data.song_id.is_some() { normal } else { disabled },
        ROW_MENU_COPY_ID,
        crate::i18n::tr("复制歌曲 ID"),
    );
    if data.queue_row {
        append_menu_item(menu, MF_SEPARATOR, 0, "");
        append_menu_item(menu, normal, ROW_MENU_CLEAR, crate::i18n::tr("清空列表"));
    }

    let command = unsafe {
        let _ = SetForegroundWindow(hwnd);
        let command = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            at.x,
            at.y,
            None,
            hwnd,
            None,
        )
        .0 as usize;
        let _ = DestroyMenu(menu);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        command
    };

    match command {
        ROW_MENU_PLAY => {
            return RowMenuOutcome::Play {
                queue_row: data.queue_row,
                index: data.index,
            };
        }
        ROW_MENU_REMOVE => return RowMenuOutcome::Remove { index: data.index },
        ROW_MENU_QUEUE_DOWNLOAD => {
            return RowMenuOutcome::QueueDownload(data.index);
        }
        ROW_MENU_MOVE_UP => {
            return RowMenuOutcome::Move {
                from: data.index,
                to: data.index.saturating_sub(1),
            };
        }
        ROW_MENU_MOVE_DOWN => {
            return RowMenuOutcome::Move {
                from: data.index,
                to: (data.index + 1).min(data.queue_rows.saturating_sub(1)),
            };
        }
        ROW_MENU_MOVE_TOP => {
            return RowMenuOutcome::Move {
                from: data.index,
                to: 0,
            };
        }
        ROW_MENU_MOVE_BOTTOM => {
            return RowMenuOutcome::Move {
                from: data.index,
                to: data.queue_rows.saturating_sub(1),
            };
        }
        ROW_MENU_QUEUE_NEXT => {
            return RowMenuOutcome::Queue {
                index: data.index,
                next: true,
            };
        }
        ROW_MENU_QUEUE_APPEND => {
            return RowMenuOutcome::Queue {
                index: data.index,
                next: false,
            };
        }
        ROW_MENU_COPY_TITLE => copy_to_clipboard(&data.title),
        ROW_MENU_COPY_ARTIST => copy_to_clipboard(&data.artist),
        ROW_MENU_COPY_PATH => {
            if let Some(path) = &data.file {
                copy_to_clipboard(&path.display().to_string());
            }
        }
        ROW_MENU_OPEN_LOCATION => {
            if let Some(path) = &data.file {
                open_file_location(path);
            }
        }
        ROW_MENU_OPEN_URL => {
            if let Some(url) = &data.url {
                let _ = webbrowser::open(url);
            }
        }
        ROW_MENU_COPY_ID => {
            if let Some(song_id) = &data.song_id {
                copy_to_clipboard(song_id);
            }
        }
        ROW_MENU_CLEAR => {
            let prompt = format!("{}?", crate::i18n::tr("清空列表"));
            let wide: Vec<u16> = prompt.encode_utf16().chain([0]).collect();
            let answer = unsafe {
                MessageBoxW(
                    Some(hwnd),
                    PCWSTR(wide.as_ptr()),
                    w!("Bamboo Music Player"),
                    MB_YESNO | MB_ICONQUESTION,
                )
            };
            if answer == IDYES {
                return RowMenuOutcome::ClearQueue;
            }
        }
        ROW_MENU_LIBRARY_REMOVE => {
            let prompt = format!(
                "{}“{}”？\r\n{}",
                crate::i18n::tr("从曲库移除"),
                data.title,
                crate::i18n::tr("只从索引移除，不会删除文件。")
            );
            let wide: Vec<u16> = prompt.encode_utf16().chain([0]).collect();
            let answer = unsafe {
                MessageBoxW(
                    Some(hwnd),
                    PCWSTR(wide.as_ptr()),
                    w!("Bamboo Music Player"),
                    MB_YESNO | MB_ICONQUESTION,
                )
            };
            if answer == IDYES {
                return RowMenuOutcome::LibraryRemove(data.index);
            }
        }
        _ => {}
    }
    RowMenuOutcome::Nothing
}

/// Show a popup menu at the cursor and return the chosen command id.
fn popup_command(hwnd: HWND, menu: HMENU) -> usize {
    let mut at = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut at);
        let _ = SetForegroundWindow(hwnd);
        let command = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            at.x,
            at.y,
            None,
            hwnd,
            None,
        )
        .0 as usize;
        let _ = DestroyMenu(menu);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        command
    }
}

fn show_smart_row_menu(hwnd: HWND, data: RowMenuData) -> RowMenuOutcome {
    if data.file.is_none() {
        return RowMenuOutcome::Nothing;
    }
    let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
        return RowMenuOutcome::Nothing;
    };
    let normal = MF_STRING;
    append_menu_item(menu, normal, ROW_MENU_PLAY, crate::i18n::tr("播放"));
    append_menu_item(menu, normal, ROW_MENU_SMART_QUEUE, crate::i18n::tr("加入播放队列"));
    append_menu_item(menu, MF_SEPARATOR, 0, "");
    append_menu_item(
        menu,
        normal,
        ROW_MENU_SMART_REMOVE,
        crate::i18n::tr("从曲库移除"),
    );
    append_menu_item(menu, MF_SEPARATOR, 0, "");
    append_menu_item(menu, normal, ROW_MENU_COPY_TITLE, crate::i18n::tr("复制歌名"));
    append_menu_item(menu, normal, ROW_MENU_COPY_ARTIST, crate::i18n::tr("复制歌手"));
    append_menu_item(menu, normal, ROW_MENU_COPY_PATH, crate::i18n::tr("复制文件路径"));
    append_menu_item(
        menu,
        normal,
        ROW_MENU_OPEN_LOCATION,
        crate::i18n::tr("打开文件位置"),
    );
    match popup_command(hwnd, menu) {
        ROW_MENU_PLAY => RowMenuOutcome::SmartPlay(data.index),
        ROW_MENU_SMART_QUEUE => RowMenuOutcome::SmartQueue(data.index),
        ROW_MENU_SMART_REMOVE => RowMenuOutcome::SmartRemove(data.index),
        ROW_MENU_COPY_TITLE => {
            copy_to_clipboard(&data.title);
            RowMenuOutcome::Nothing
        }
        ROW_MENU_COPY_ARTIST => {
            copy_to_clipboard(&data.artist);
            RowMenuOutcome::Nothing
        }
        ROW_MENU_COPY_PATH => {
            if let Some(path) = &data.file {
                copy_to_clipboard(&path.display().to_string());
            }
            RowMenuOutcome::Nothing
        }
        ROW_MENU_OPEN_LOCATION => {
            if let Some(path) = &data.file {
                open_file_location(path);
            }
            RowMenuOutcome::Nothing
        }
        _ => RowMenuOutcome::Nothing,
    }
}

fn show_duplicate_row_menu(hwnd: HWND, data: RowMenuData) -> RowMenuOutcome {
    let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
        return RowMenuOutcome::Nothing;
    };
    let normal = MF_STRING;
    if data.header {
        append_menu_item(
            menu,
            normal,
            ROW_MENU_DUP_KEEP,
            crate::i18n::tr("仅保留第一项（从索引移除其余）"),
        );
        append_menu_item(
            menu,
            normal,
            ROW_MENU_DUP_REMOVE_GROUP,
            crate::i18n::tr("从索引移除整组"),
        );
    } else if data.file.is_some() {
        append_menu_item(menu, normal, ROW_MENU_PLAY, crate::i18n::tr("播放"));
        append_menu_item(
            menu,
            normal,
            ROW_MENU_LIBRARY_REMOVE,
            crate::i18n::tr("从索引移除"),
        );
        append_menu_item(menu, MF_SEPARATOR, 0, "");
        append_menu_item(menu, normal, ROW_MENU_COPY_TITLE, crate::i18n::tr("复制歌名"));
        append_menu_item(menu, normal, ROW_MENU_COPY_PATH, crate::i18n::tr("复制文件路径"));
        append_menu_item(
            menu,
            normal,
            ROW_MENU_OPEN_LOCATION,
            crate::i18n::tr("打开文件位置"),
        );
    } else {
        unsafe { let _ = DestroyMenu(menu); }
        return RowMenuOutcome::Nothing;
    }
    match popup_command(hwnd, menu) {
        ROW_MENU_DUP_KEEP => RowMenuOutcome::DuplicateKeep(data.index),
        ROW_MENU_DUP_REMOVE_GROUP => RowMenuOutcome::DuplicateRemoveGroup(data.index),
        ROW_MENU_PLAY => RowMenuOutcome::DuplicatePlay(data.index),
        ROW_MENU_LIBRARY_REMOVE => RowMenuOutcome::DuplicateRemove(data.index),
        ROW_MENU_COPY_TITLE => {
            copy_to_clipboard(&data.title);
            RowMenuOutcome::Nothing
        }
        ROW_MENU_COPY_PATH => {
            if let Some(path) = &data.file {
                copy_to_clipboard(&path.display().to_string());
            }
            RowMenuOutcome::Nothing
        }
        ROW_MENU_OPEN_LOCATION => {
            if let Some(path) = &data.file {
                open_file_location(path);
            }
            RowMenuOutcome::Nothing
        }
        _ => RowMenuOutcome::Nothing,
    }
}

fn show_online_row_menu(hwnd: HWND, data: RowMenuData) -> RowMenuOutcome {
    let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
        return RowMenuOutcome::Nothing;
    };
    let normal = MF_STRING;
    let disabled = MF_STRING | MF_GRAYED;
    if data.album_result {
        append_menu_item(
            menu,
            normal,
            ROW_MENU_ONLINE_ALBUM,
            crate::i18n::tr("打开专辑"),
        );
        append_menu_item(menu, MF_SEPARATOR, 0, "");
        append_menu_item(menu, normal, ROW_MENU_COPY_TITLE, crate::i18n::tr("复制专辑名"));
        append_menu_item(menu, normal, ROW_MENU_COPY_ARTIST, crate::i18n::tr("复制歌手"));
        return match popup_command(hwnd, menu) {
            ROW_MENU_ONLINE_ALBUM => data
                .album_id
                .clone()
                .map_or(RowMenuOutcome::Nothing, RowMenuOutcome::OnlineAlbum),
            ROW_MENU_COPY_TITLE => {
                copy_to_clipboard(&data.title);
                RowMenuOutcome::Nothing
            }
            ROW_MENU_COPY_ARTIST => {
                copy_to_clipboard(&data.artist);
                RowMenuOutcome::Nothing
            }
            _ => RowMenuOutcome::Nothing,
        };
    }
    append_menu_item(menu, normal, ROW_MENU_PLAY, crate::i18n::tr("播放"));
    append_menu_item(menu, normal, ROW_MENU_ONLINE_QUEUE, crate::i18n::tr("加入播放队列"));
    append_menu_item(menu, normal, ROW_MENU_ONLINE_DOWNLOAD, crate::i18n::tr("下载"));
    append_menu_item(menu, MF_SEPARATOR, 0, "");
    append_menu_item(
        menu,
        if data.album_id.is_some() { normal } else { disabled },
        ROW_MENU_ONLINE_ALBUM,
        crate::i18n::tr("打开专辑"),
    );
    append_menu_item(
        menu,
        if data.artist_id.is_some() { normal } else { disabled },
        ROW_MENU_ONLINE_ARTIST,
        crate::i18n::tr("打开歌手"),
    );
    append_menu_item(menu, MF_SEPARATOR, 0, "");
    append_menu_item(menu, normal, ROW_MENU_COPY_TITLE, crate::i18n::tr("复制歌名"));
    append_menu_item(menu, normal, ROW_MENU_COPY_ARTIST, crate::i18n::tr("复制歌手"));
    append_menu_item(
        menu,
        if data.song_id.is_some() { normal } else { disabled },
        ROW_MENU_COPY_ID,
        crate::i18n::tr("复制歌曲 ID"),
    );
    match popup_command(hwnd, menu) {
        ROW_MENU_PLAY => RowMenuOutcome::OnlinePlay(data.index),
        ROW_MENU_ONLINE_QUEUE => RowMenuOutcome::OnlineQueue(data.index),
        ROW_MENU_ONLINE_DOWNLOAD => RowMenuOutcome::OnlineDownload(data.index),
        ROW_MENU_ONLINE_ALBUM => data
            .album_id
            .clone()
            .map_or(RowMenuOutcome::Nothing, RowMenuOutcome::OnlineAlbum),
        ROW_MENU_ONLINE_ARTIST => data
            .artist_id
            .clone()
            .map_or(RowMenuOutcome::Nothing, RowMenuOutcome::OnlineArtist),
        ROW_MENU_COPY_TITLE => {
            copy_to_clipboard(&data.title);
            RowMenuOutcome::Nothing
        }
        ROW_MENU_COPY_ARTIST => {
            copy_to_clipboard(&data.artist);
            RowMenuOutcome::Nothing
        }
        ROW_MENU_COPY_ID => {
            if let Some(song_id) = &data.song_id {
                copy_to_clipboard(song_id);
            }
            RowMenuOutcome::Nothing
        }
        _ => RowMenuOutcome::Nothing,
    }
}

/// Context menu for one folder-browser row (folder / audio / playlist).
/// Context menu for one discover row.
fn show_discover_row_menu(hwnd: HWND, data: RowMenuData) -> RowMenuOutcome {
    let mut at = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut at);
    }
    let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
        return RowMenuOutcome::Nothing;
    };
    let normal = MF_STRING;
    append_menu_item(menu, normal, ROW_MENU_PLAY, crate::i18n::tr("播放"));
    append_menu_item(menu, normal, ROW_MENU_ONLINE_QUEUE, crate::i18n::tr("加入播放队列"));
    append_menu_item(menu, normal, ROW_MENU_ONLINE_DOWNLOAD, crate::i18n::tr("下载"));
    append_menu_item(menu, MF_SEPARATOR, 0, "");
    append_menu_item(menu, normal, ROW_MENU_COPY_TITLE, crate::i18n::tr("复制歌名"));
    append_menu_item(menu, normal, ROW_MENU_COPY_ARTIST, crate::i18n::tr("复制歌手"));
    let command = unsafe {
        let _ = SetForegroundWindow(hwnd);
        let command = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            at.x,
            at.y,
            None,
            hwnd,
            None,
        )
        .0 as usize;
        let _ = DestroyMenu(menu);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        command
    };
    match command {
        ROW_MENU_PLAY => RowMenuOutcome::DiscoverPlay(data.index),
        ROW_MENU_ONLINE_QUEUE => RowMenuOutcome::DiscoverQueue(data.index),
        ROW_MENU_ONLINE_DOWNLOAD => RowMenuOutcome::DiscoverDownload(data.index),
        ROW_MENU_COPY_TITLE => {
            copy_to_clipboard(&data.title);
            RowMenuOutcome::Nothing
        }
        ROW_MENU_COPY_ARTIST => {
            copy_to_clipboard(&data.artist);
            RowMenuOutcome::Nothing
        }
        _ => RowMenuOutcome::Nothing,
    }
}

fn show_folder_row_menu(hwnd: HWND, data: RowMenuData) -> RowMenuOutcome {
    let mut at = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut at);
    }
    let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
        return RowMenuOutcome::Nothing;
    };
    let normal = MF_STRING;
    let disabled = MF_STRING | MF_GRAYED;
    let play_label = if data.folder_is_dir {
        "打开文件夹"
    } else if data.folder_is_playlist {
        "导入播放列表"
    } else {
        "播放"
    };
    append_menu_item(
        menu,
        if data.file.is_some() { normal } else { disabled },
        ROW_MENU_PLAY,
        crate::i18n::tr(play_label),
    );
    if data.folder_is_dir {
        append_menu_item(
            menu,
            normal,
            ROW_MENU_ADD_LIBRARY,
            crate::i18n::tr("加入曲库"),
        );
    } else if !data.folder_is_playlist {
        // Importing a playlist replaces the queue, so "add to queue" is only
        // offered for plain audio files.
        append_menu_item(
            menu,
            normal,
            ROW_MENU_QUEUE_APPEND,
            crate::i18n::tr("添加到播放队列"),
        );
    }
    append_menu_item(menu, MF_SEPARATOR, 0, "");
    append_menu_item(
        menu,
        if data.file.is_some() { normal } else { disabled },
        ROW_MENU_COPY_PATH,
        crate::i18n::tr("复制路径"),
    );
    let open_label = if data.folder_is_dir {
        "在资源管理器中打开"
    } else {
        "打开文件位置"
    };
    append_menu_item(
        menu,
        if data.file.is_some() { normal } else { disabled },
        ROW_MENU_OPEN_LOCATION,
        crate::i18n::tr(open_label),
    );

    let command = unsafe {
        let _ = SetForegroundWindow(hwnd);
        let command = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            at.x,
            at.y,
            None,
            hwnd,
            None,
        )
        .0 as usize;
        let _ = DestroyMenu(menu);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        command
    };

    match command {
        ROW_MENU_PLAY => return RowMenuOutcome::FolderPlay(data.index),
        ROW_MENU_QUEUE_APPEND => return RowMenuOutcome::FolderEnqueue(data.index),
        ROW_MENU_ADD_LIBRARY => {
            if let Some(path) = data.file {
                return RowMenuOutcome::FolderAddLibrary(path);
            }
        }
        ROW_MENU_COPY_PATH => {
            if let Some(path) = &data.file {
                copy_to_clipboard(&path.display().to_string());
            }
        }
        ROW_MENU_OPEN_LOCATION => {
            if let Some(path) = &data.file {
                if data.folder_is_dir {
                    open_path(path);
                } else {
                    open_file_location(path);
                }
            }
        }
        _ => {}
    }
    RowMenuOutcome::Nothing
}

fn apply_row_menu(hwnd: HWND, outcome: RowMenuOutcome) {
    let Some(state) = (unsafe { state_mut(hwnd) }) else {
        return;
    };
    match outcome {
        RowMenuOutcome::Nothing => return,
        RowMenuOutcome::Play { queue_row, index } => {
            if queue_row {
                state.app.play_queue_row(hwnd, index);
            } else {
                state.app.play_library_row(hwnd, index);
            }
        }
        RowMenuOutcome::Remove { index } => {
            state
                .app
                .core
                .handle(crate::core::app::Command::RemoveFromQueue(index));
            state.app.refresh_queue_view();
            state.app.invalidate_all(hwnd);
        }
        RowMenuOutcome::Move { from, to } => {
            state
                .app
                .core
                .handle(crate::core::app::Command::QueueMove { from, to });
            state.app.refresh_queue_view();
            state.app.invalidate(hwnd, state.app.layout.list);
            state.app.invalidate(hwnd, state.app.layout.scrollbar);
        }
        RowMenuOutcome::Queue { index, next } => {
            state
                .app
                .core
                .handle(crate::core::app::Command::LibraryQueue { index, next });
            state.app.refresh_queue_view();
            state.app.invalidate_all(hwnd);
        }
        RowMenuOutcome::ClearQueue => {
            state
                .app
                .core
                .handle(crate::core::app::Command::ClearQueue);
            state.app.refresh_queue_view();
            state.app.invalidate_after_modal(hwnd);
        }
        RowMenuOutcome::FolderPlay(index) => {
            let is_dir = state
                .app
                .core
                .snapshot()
                .browser
                .entries
                .get(index)
                .map(|entry| entry.is_dir)
                .unwrap_or(false);
            state
                .app
                .core
                .handle(crate::core::app::Command::BrowsePlay(index));
            if is_dir {
                state.app.reset_folder_view(hwnd);
            }
        }
        RowMenuOutcome::FolderEnqueue(index) => {
            state
                .app
                .core
                .handle(crate::core::app::Command::BrowseEnqueue(index));
            state.app.invalidate(hwnd, state.app.layout.header);
        }
        RowMenuOutcome::FolderAddLibrary(path) => {
            state
                .app
                .core
                .handle(crate::core::app::Command::LibraryAddFolder(path));
            state.app.core.flush_settings();
        }
        RowMenuOutcome::LibraryRemove(index) => {
            state
                .app
                .core
                .handle(crate::core::app::Command::LibraryRemove(index));
            // The DB worker answers with Changed; repaint now so the removed
            // row disappears even while playback is paused (no timer).
            state.app.invalidate_after_modal(hwnd);
        }
        RowMenuOutcome::OnlinePlay(index) => {
            state
                .app
                .core
                .handle(crate::core::app::Command::OnlinePlay(index));
            state.app.invalidate_all(hwnd);
        }
        RowMenuOutcome::OnlineQueue(index) => {
            state
                .app
                .core
                .handle(crate::core::app::Command::OnlineQueue(index));
            state.app.invalidate(hwnd, state.app.layout.header);
        }
        RowMenuOutcome::OnlineDownload(index) => {
            state
                .app
                .core
                .handle(crate::core::app::Command::OnlineDownload(index));
            state.app.invalidate(hwnd, state.app.layout.header);
        }
        RowMenuOutcome::DiscoverPlay(index) => {
            state
                .app
                .core
                .handle(crate::core::app::Command::DiscoverPlay(index));
            state.app.invalidate_all(hwnd);
        }
        RowMenuOutcome::DiscoverQueue(index) => {
            state
                .app
                .core
                .handle(crate::core::app::Command::DiscoverQueue(index));
            state.app.invalidate(hwnd, state.app.layout.header);
        }
        RowMenuOutcome::DiscoverDownload(index) => {
            state
                .app
                .core
                .handle(crate::core::app::Command::DiscoverDownload(index));
            state.app.invalidate(hwnd, state.app.layout.header);
        }
        RowMenuOutcome::QueueDownload(index) => {
            state
                .app
                .core
                .handle(crate::core::app::Command::DownloadQueue(index));
            state.app.invalidate(hwnd, state.app.layout.header);
        }
        RowMenuOutcome::OnlineAlbum(album_id) => {
            state
                .app
                .core
                .handle(crate::core::app::Command::OnlineOpenAlbum(album_id));
            state.app.reset_list_scroll(hwnd);
            state.app.invalidate_all(hwnd);
        }
        RowMenuOutcome::OnlineArtist(artist_id) => {
            state
                .app
                .core
                .handle(crate::core::app::Command::OnlineOpenArtist(artist_id));
            state.app.reset_list_scroll(hwnd);
            state.app.invalidate_all(hwnd);
        }
        RowMenuOutcome::SmartPlay(index) => {
            state
                .app
                .core
                .handle(crate::core::app::Command::LibraryPlaySmart(index));
            state.app.invalidate_all(hwnd);
        }
        RowMenuOutcome::SmartQueue(index) => {
            state
                .app
                .core
                .handle(crate::core::app::Command::LibraryQueueSmart(index));
            state.app.invalidate(hwnd, state.app.layout.header);
        }
        RowMenuOutcome::SmartRemove(index) => {
            state
                .app
                .core
                .handle(crate::core::app::Command::LibraryRemoveSmart(index));
            state.app.invalidate_all(hwnd);
        }
        RowMenuOutcome::DuplicateKeep(index) => {
            state
                .app
                .core
                .handle(crate::core::app::Command::LibraryKeepDuplicateGroup(index));
            state.app.invalidate_all(hwnd);
        }
        RowMenuOutcome::DuplicateRemoveGroup(index) => {
            state
                .app
                .core
                .handle(crate::core::app::Command::LibraryRemoveDuplicateGroup(index));
            state.app.invalidate_all(hwnd);
        }
        RowMenuOutcome::DuplicateRemove(index) => {
            state
                .app
                .core
                .handle(crate::core::app::Command::LibraryRemoveDuplicate(index));
            state.app.invalidate_all(hwnd);
        }
        RowMenuOutcome::DuplicatePlay(index) => {
            state
                .app
                .core
                .handle(crate::core::app::Command::LibraryPlayDuplicate(index));
            state.app.invalidate_all(hwnd);
        }
    }
    sync_lyrics(state, hwnd);
    sync_panel(state, hwnd);
    sync_edit_boxes(state, hwnd);
    unsafe { sync_timer(hwnd) };
}

fn mem_stats() -> (f64, f64) {
    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ..Default::default()
    };
    match unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) } {
        Ok(()) => (
            counters.PagefileUsage as f64 / (1024.0 * 1024.0),
            counters.WorkingSetSize as f64 / (1024.0 * 1024.0),
        ),
        Err(_) => (0.0, 0.0),
    }
}

// ---------------------------------------------------------------------------
// Track context menu (right click on the player-bar / card track info)
// ---------------------------------------------------------------------------

const MENU_COPY_TITLE: usize = 1;
const MENU_COPY_ARTIST: usize = 2;
const MENU_OPEN_LOCATION: usize = 3;
const MENU_OPEN_URL: usize = 4;
const MENU_COPY_ID: usize = 5;
/// CF_UNICODETEXT from Win32; kept numeric to avoid enabling the Ole module.
const CF_UNICODETEXT_FORMAT: u32 = 13;

struct TrackMenuData {
    title: String,
    artist: String,
    file: Option<std::path::PathBuf>,
    url: Option<String>,
    song_id: Option<String>,
}

impl TrackMenuData {
    fn from_track(track: &Track) -> Self {
        let (file, url, song_id) = match &track.source {
            TrackSource::Local { path } => (Some(path.clone()), None, None),
            TrackSource::Url { url } | TrackSource::Radio { url } => {
                (None, Some(url.clone()), None)
            }
            TrackSource::Bamboo { song_id, .. } => (None, None, Some(song_id.clone())),
        };
        Self {
            title: track.title.clone(),
            artist: track.artist.clone(),
            file,
            url,
            song_id,
        }
    }
}

fn copy_to_clipboard(text: &str) {
    if text.is_empty() {
        return;
    }
    unsafe {
        if OpenClipboard(None).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
        if let Ok(memory) = GlobalAlloc(GMEM_MOVEABLE, wide.len() * size_of::<u16>()) {
            let pointer = GlobalLock(memory) as *mut u16;
            if pointer.is_null() {
                let _ = GlobalFree(Some(memory));
            } else {
                std::ptr::copy_nonoverlapping(wide.as_ptr(), pointer, wide.len());
                let _ = GlobalUnlock(memory);
                // Ownership transfers to the clipboard on success.
                if SetClipboardData(CF_UNICODETEXT_FORMAT, Some(HANDLE(memory.0))).is_err() {
                    let _ = GlobalFree(Some(memory));
                }
            }
        }
        let _ = CloseClipboard();
    }
}

fn open_file_location(path: &std::path::Path) {
    let params = format!("/select,\"{}\"", path.display());
    let wide: Vec<u16> = params.encode_utf16().chain([0]).collect();
    unsafe {
        let _ = ShellExecuteW(
            None,
            w!("open"),
            w!("explorer.exe"),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

/// Open a directory (or any shell target) with its default handler.
fn open_path(path: &std::path::Path) {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    unsafe {
        let _ = ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

fn append_menu_item(menu: HMENU, flags: windows::Win32::UI::WindowsAndMessaging::MENU_ITEM_FLAGS, id: usize, label: &str) {
    let wide: Vec<u16> = label.encode_utf16().chain([0]).collect();
    unsafe {
        let _ = AppendMenuW(menu, flags, id, PCWSTR(wide.as_ptr()));
    }
}

/// Show the current-track menu at screen position `at`. Must run without a
/// live `&mut State` borrow: `TrackPopupMenu` pumps a nested message loop.
fn show_track_menu(hwnd: HWND, data: TrackMenuData, at: POINT) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        let normal = MF_STRING;
        let disabled = MF_STRING | MF_GRAYED;
        append_menu_item(
            menu,
            if data.title.is_empty() { disabled } else { normal },
            MENU_COPY_TITLE,
            crate::i18n::tr("复制歌名"),
        );
        append_menu_item(
            menu,
            if data.artist.is_empty() { disabled } else { normal },
            MENU_COPY_ARTIST,
            crate::i18n::tr("复制歌手"),
        );
        append_menu_item(menu, MF_SEPARATOR, 0, "");
        append_menu_item(
            menu,
            if data.file.is_some() { normal } else { disabled },
            MENU_OPEN_LOCATION,
            crate::i18n::tr("打开文件位置"),
        );
        append_menu_item(
            menu,
            if data.url.is_some() { normal } else { disabled },
            MENU_OPEN_URL,
            crate::i18n::tr("打开 URL"),
        );
        append_menu_item(
            menu,
            if data.song_id.is_some() { normal } else { disabled },
            MENU_COPY_ID,
            crate::i18n::tr("复制歌曲 ID"),
        );

        let _ = SetForegroundWindow(hwnd);
        let command = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            at.x,
            at.y,
            None,
            hwnd,
            None,
        )
        .0 as usize;
        let _ = DestroyMenu(menu);
        // Required workaround: the menu loop leaves the window in a state where
        // it does not dismiss on the next outside click.
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));

        match command {
            MENU_COPY_TITLE => copy_to_clipboard(&data.title),
            MENU_COPY_ARTIST => copy_to_clipboard(&data.artist),
            MENU_OPEN_LOCATION => {
                if let Some(path) = &data.file {
                    open_file_location(path);
                }
            }
            MENU_OPEN_URL => {
                if let Some(url) = &data.url {
                    let _ = webbrowser::open(url);
                }
            }
            MENU_COPY_ID => {
                if let Some(song_id) = &data.song_id {
                    copy_to_clipboard(song_id);
                }
            }
            _ => {}
        }
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let _ = unsafe { BeginPaint(hwnd, &mut ps) };
            if let Some(state) = unsafe { state_mut(hwnd) } {
                let scale = state.app.dpi / 96.0;
                let bounds = Rect::new(
                    ps.rcPaint.left as f32 / scale,
                    ps.rcPaint.top as f32 / scale,
                    ps.rcPaint.right as f32 / scale,
                    ps.rcPaint.bottom as f32 / scale,
                );
                let mut rects = state.app.take_dirty();
                if rects.is_empty() {
                    rects.push(bounds);
                }
                let started = Instant::now();
                match state.painter.paint(hwnd, ps.hdc, &state.app, rects) {
                    Ok(()) => state.painted = true,
                    Err(err) => warn!("paint failed: {err}"),
                }
                state.app.record_frame(started.elapsed().as_nanos());
            }
            let _ = unsafe { EndPaint(hwnd, &ps) };
            LRESULT(0)
        }
        WM_SHOWWINDOW => {
            if wparam.0 != 0
                && let Some(state) = unsafe { state_mut(hwnd) }
            {
                restore_window(state, hwnd);
            }
            LRESULT(0)
        }
        WM_SIZE => {
            if let Some(state) = unsafe { state_mut(hwnd) } {
                let mut client = RECT::default();
                let _ = unsafe { GetClientRect(hwnd, &mut client) };
                let width = (client.right - client.left).max(1) as u32;
                let height = (client.bottom - client.top).max(1) as u32;
                let dpi = unsafe { GetDpiForWindow(hwnd) };
                let dpi = if dpi > 0 { dpi as f32 } else { 96.0 };
                state.app.set_metrics(width, height, dpi);
                state.painter.resize(width, height, dpi);
                sync_lyrics(state, hwnd);
                sync_panel(state, hwnd);
                sync_edit_boxes(state, hwnd);
                state.app.invalidate_all(hwnd);
                // Restoring from the taskbar while a stale occlusion poll had
                // marked the iconic window as fully covered: without this the
                // full invalidation above is dropped and the window stays
                // blank/stale until the next animation poll.
                if !state.app.window_visible && !unsafe { IsIconic(hwnd) }.as_bool() {
                    state.app.restore_visibility(hwnd);
                }
            }
            unsafe { sync_timer(hwnd) };
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let suggested = unsafe { &*(lparam.0 as *const RECT) };
            let _ = unsafe {
                SetWindowPos(
                    hwnd,
                    None,
                    suggested.left,
                    suggested.top,
                    suggested.right - suggested.left,
                    suggested.bottom - suggested.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                )
            };
            if let Some(state) = unsafe { state_mut(hwnd) } {
                state.app.reset_visibility();
                state.painter.dpi_changed();
                let dpi = unsafe { GetDpiForWindow(hwnd) } as f32;
                let dpi = if dpi > 0.0 { dpi } else { 96.0 };
                            let font = create_edit_font(dpi);
                unsafe {
                    for edit in [
                        state.edit,
                        state.edit_extensions,
                        state.edit_number,
                        state.edit_folder,
                    ] {
                        SendMessageW(
                            edit,
                            WM_SETFONT,
                            Some(WPARAM(font.0 as usize)),
                            Some(LPARAM(1)),
                        );
                        set_edit_margins(edit, dpi);
                    }
                    let _ = DeleteObject(HGDIOBJ(state.edit_font.0));
                    let item_height =
                        (crate::layout::SORT_ITEM_HEIGHT * dpi / 96.0).round() as isize;
                    SendMessageW(
                        state.sort_list,
                        LB_SETITEMHEIGHT,
                        Some(WPARAM(0)),
                        Some(LPARAM(item_height)),
                    );
                    SendMessageW(
                        state.sort_list,
                        WM_SETFONT,
                        Some(WPARAM(font.0 as usize)),
                        Some(LPARAM(1)),
                    );
                }
                state.edit_font = font;
                sync_edit_boxes(state, hwnd);
                sync_lyrics(state, hwnd);
                sync_panel(state, hwnd);
                state.app.invalidate_all(hwnd);
            }
            LRESULT(0)
        }
        WM_GETMINMAXINFO => {
            if let Some(state) = unsafe { state_mut(hwnd) } {
                let scale = state.app.dpi / 96.0;
                let info = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
                // Also fits 175% DPI laptops with a small work area; the layout
                // is designed down to 640x460 DIP.
                info.ptMinTrackSize.x = (640.0 * scale) as i32;
                info.ptMinTrackSize.y = (460.0 * scale) as i32;
            }
            LRESULT(0)
        }
        WM_DEVICECHANGE => {
            // Only used to notice a controller being plugged in/out; there is
            // no polling timer while no gamepad is connected.
            if let Some(state) = unsafe { state_mut(hwnd) } {
                state.gamepad.refresh_connection();
                sync_gamepad(state, hwnd);
            }
            LRESULT(0)
        }
        WM_GETOBJECT => {
            if lparam.0 as i32 == crate::accessibility::root_object_id() {
                return crate::accessibility::return_root(hwnd, wparam, lparam);
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        WM_ACTIVATE => {
            if let Some(state) = unsafe { state_mut(hwnd) } {
                if wparam.0 == 0 {
                    // Dropping to the background: the hover target and its
                    // highlight belong to a window that is no longer active.
                    state.app.clear_hover(hwnd);
                    clear_row_tooltip(hwnd, state);
                } else {
                    // Back in the foreground. The occlusion poll only runs on
                    // animation ticks (and up to 20 of them at the unfocused
                    // 1 s heartbeat), so every cached exposed rectangle is
                    // stale here: hover paint was being clipped against the
                    // old z-order for seconds. Reset the cache and repaint
                    // the whole client in one pass instead.
                    restore_window(state, hwnd);
                }
            }
            // Focus changes start/stop animations when "pause when unfocused"
            // is enabled; the timer is restarted without repainting by itself.
            unsafe { sync_timer(hwnd) };
            LRESULT(0)
        }
        WM_TIMER => {
            match wparam.0 {
                ANIM_TIMER => {
                    let mut timer_changed = false;
                    if let Some(state) = unsafe { state_mut(hwnd) } {
                        let now = Instant::now();
                        let had_toast = state.app.toast_active();
                        state.app.tick(hwnd);
                        timer_changed = had_toast && !state.app.toast_active();
                        if state.app.window_visible {
                            if state.app.visibility_changed {
                                state.app.visibility_changed = false;
                                state.lyrics.mark_dirty();
                            }
                            sync_panel(state, hwnd);
                        }
                        // The embedded child hides itself when the player is
                        // not visible; a standalone lyrics window keeps
                        // following playback on its own.
                        sync_lyrics(state, hwnd);
                        // Overlays and the shell integration are heartbeats,
                        // not animation: 20 Hz and 4 Hz are enough.
                        if now.duration_since(state.last_overlay_sync) >= OVERLAY_SYNC_INTERVAL {
                            state.last_overlay_sync = now;
                            sync_overlays(state, hwnd);
                        }
                        if now.duration_since(state.last_system_sync) >= SYSTEM_SYNC_INTERVAL {
                            state.last_system_sync = now;
                            sync_system(state, hwnd);
                        }
                    }
                    if timer_changed {
                        unsafe { sync_timer(hwnd) };
                    }
                }
                QUIT_TIMER => unsafe { PostQuitMessage(0) },
                SEARCH_TIMER => {
                    let _ = unsafe { KillTimer(Some(hwnd), SEARCH_TIMER) };
                    if let Some(state) = unsafe { state_mut(hwnd) } {
                        if state.app.page == crate::app::Page::Search {
                            let query = state.app.core.snapshot().online.query.clone();
                            let limit = state.app.online_page_size() as u32;
                            state.app.core.handle(
                                crate::core::app::Command::OnlineSearch {
                                    query,
                                    offset: 0,
                                    limit,
                                },
                            );
                        } else {
                            state
                                .app
                                .core
                                .handle(crate::core::app::Command::LibraryRefresh);
                        }
                        state.app.invalidate(hwnd, state.app.layout.list);
                        state.app.invalidate(hwnd, state.app.layout.header);
                    }
                }
                EXTENSIONS_TIMER => {
                    let _ = unsafe { KillTimer(Some(hwnd), EXTENSIONS_TIMER) };
                    if let Some(state) = unsafe { state_mut(hwnd) } {
                        let text = edit_text(state.edit_extensions);
                        state
                            .app
                            .core
                            .handle(crate::core::app::Command::LibrarySetExtensions(text));
                        state.app.invalidate_all(hwnd);
                    }
                }
                TOOLTIP_TIMER => {
                    let _ = unsafe { KillTimer(Some(hwnd), TOOLTIP_TIMER) };
                    if let Some(state) = unsafe { state_mut(hwnd) } {
                        show_row_tooltip(hwnd, state);
                    }
                }
                TOOLTIP_HIDE_TIMER => {
                    let _ = unsafe { KillTimer(Some(hwnd), TOOLTIP_HIDE_TIMER) };
                    if let Some(state) = unsafe { state_mut(hwnd) } {
                        hide_row_tooltip(hwnd, state);
                    }
                }
                IDLE_TRIM_TIMER => {
                    if let Some(state) = unsafe { state_mut(hwnd) } {
                        let idle = state.app.core.settings.window.trim_idle_secs;
                        let hidden = !unsafe { IsWindowVisible(hwnd) }.as_bool()
                            || unsafe { IsIconic(hwnd) }.as_bool();
                        // Keep a visible window responsive: only trim after a
                        // long idle period, while a hidden/minimized player can
                        // release memory after the configured delay.
                        let required = if hidden { idle } else { idle.max(120) };
                        let now = Instant::now();
                        if now.duration_since(state.last_input).as_secs() >= required
                            && now.duration_since(state.last_trim).as_secs() >= required
                        {
                            state.last_trim = now;
                            crate::core::platform::memory::trim_working_set();
                        }
                    }
                }
                GAMEPAD_TIMER => {
                    let mut changed = false;
                    let mut connected = true;
                    if let Some(state) = unsafe { state_mut(hwnd) } {
                        let actions = state.gamepad.poll();
                        connected = state.gamepad.is_connected();
                        changed = !actions.is_empty();
                        for action in actions {
                            perform_gamepad_action(state, hwnd, action);
                        }
                    }
                    if !connected {
                        let _ = unsafe { KillTimer(Some(hwnd), GAMEPAD_TIMER) };
                        if let Some(state) = unsafe { state_mut(hwnd) } {
                            state.gamepad_running = false;
                        }
                    }
                    if changed {
                        if let Some(state) = unsafe { state_mut(hwnd) } {
                            sync_panel(state, hwnd);
                            sync_lyrics(state, hwnd);
                            sync_overlays(state, hwnd);
                        }
                        unsafe { sync_timer(hwnd) };
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            if let Some(state) = unsafe { state_mut(hwnd) } {
                state.last_input = Instant::now();
                // A click on the parent means the search box is not the target.
                let _ = unsafe { SetFocus(Some(hwnd)) };
                hide_row_tooltip(hwnd, state);
                let (x, y) = message_point(lparam, state.app.dpi);
                let over_sort = state.app.page == crate::app::Page::Library
                    && state.app.layout.sort.inflate(2.0).contains(x, y);
                state.app.on_press(hwnd, x, y);
                if over_sort {
                    let open = !state.app.sort_menu_open;
                    state.app.set_sort_menu_open(hwnd, open);
                } else {
                    if state.app.sort_menu_open {
                        state.app.set_sort_menu_open(hwnd, false);
                    }
                    state.app.on_click(hwnd, x, y);
                    sync_lyrics(state, hwnd);
                    sync_panel(state, hwnd);
                }
                sync_edit_boxes(state, hwnd);
                sync_inline_edits(state, hwnd);
                sync_overlays(state, hwnd);
                sync_system(state, hwnd);
            }
            // Frame-rate changes restart the animation timer immediately.
            unsafe { sync_timer(hwnd) };
            // The folder picker is modal and pumps messages; run it only after
            // the `State` borrow above has ended.
            let toggle_window = unsafe { state_mut(hwnd) }
                .map(|state| state.app.take_lyrics_window_toggle())
                .unwrap_or(false);
            if toggle_window {
                toggle_lyrics_window(hwnd);
            }
            let open_editor = unsafe { state_mut(hwnd) }
                .map(|state| state.app.take_lyrics_editor())
                .unwrap_or(false);
            if open_editor {
                open_lyrics_editor(hwnd);
            }
            let queue_import = unsafe { state_mut(hwnd) }
                .map(|state| state.app.take_queue_import())
                .unwrap_or(false);
            let queue_clear = unsafe { state_mut(hwnd) }
                .map(|state| state.app.take_queue_clear())
                .unwrap_or(false);
            let stats_reset = unsafe { state_mut(hwnd) }
                .map(|state| state.app.take_stats_reset())
                .unwrap_or(false);
            if stats_reset {
                let prompt: Vec<u16> = crate::i18n::tr(
                    "清除全部播放次数与最近播放记录？\r\n曲库文件不会被删除。",
                )
                .encode_utf16()
                .chain([0])
                .collect();
                let answer = unsafe {
                    MessageBoxW(
                        Some(hwnd),
                        PCWSTR(prompt.as_ptr()),
                        w!("Bamboo Music Player"),
                        MB_YESNO | MB_ICONWARNING,
                    )
                };
                if answer == IDYES
                    && let Some(state) = unsafe { state_mut(hwnd) }
                {
                    state
                        .app
                        .core
                        .handle(crate::core::app::Command::LibraryResetStats);
                    state.app.invalidate_all(hwnd);
                }
            }
            let export_data = unsafe { state_mut(hwnd) }.and_then(|state| {
                if !state.app.take_queue_export() {
                    return None;
                }
                let snapshot = state.app.core.snapshot();
                if snapshot.queue.is_empty() {
                    return None;
                }
                Some((
                    snapshot.queue.to_vec(),
                    state.app.core.settings.playlist.export_utf8_bom,
                ))
            });
            if queue_import {
                pick_playlist(hwnd);
            }
            if let Some((tracks, bom)) = export_data
                && let Some(mut path) = rfd::FileDialog::new()
                    .add_filter("Bamboo Playlist", &["mp3list"])
                    .set_file_name("playlist.mp3list")
                    .save_file()
            {
                if path.extension().is_none() {
                    path.set_extension("mp3list");
                }
                let result = crate::playlist::mp3list::export(&path, &tracks, bom);
                if let Some(state) = unsafe { state_mut(hwnd) } {
                    match result {
                        Ok(count) => state.app.push_toast(
                            hwnd,
                            crate::trf!("歌单已导出：{} 首", count),
                            false,
                        ),
                        Err(err) => state.app.push_toast(
                            hwnd,
                            crate::trf!("导出歌单失败：{}", err),
                            true,
                        ),
                    }
                    unsafe { sync_timer(hwnd) };
                }
            }
            if queue_clear {
                let prompt = format!("{}?", crate::i18n::tr("清空列表"));
                let wide: Vec<u16> = prompt.encode_utf16().chain([0]).collect();
                let answer = unsafe {
                    MessageBoxW(
                        Some(hwnd),
                        PCWSTR(wide.as_ptr()),
                        w!("Bamboo Music Player"),
                        MB_YESNO | MB_ICONQUESTION,
                    )
                };
                if answer == IDYES
                    && let Some(state) = unsafe { state_mut(hwnd) }
                {
                    state
                        .app
                        .core
                        .handle(crate::core::app::Command::ClearQueue);
                    state.app.refresh_queue_view();
                    state.app.invalidate_after_modal(hwnd);
                    state
                        .app
                        .push_toast(hwnd, crate::i18n::tr("播放队列已清空"), false);
                    sync_lyrics(state, hwnd);
                    sync_panel(state, hwnd);
                    unsafe { sync_timer(hwnd) };
                }
            }
            let pick = unsafe { state_mut(hwnd) }
                .map(|state| state.app.take_pick_folder())
                .unwrap_or(false);
            if pick {
                if let Some(folder) = rfd::FileDialog::new().pick_folder()
                    && let Some(state) = unsafe { state_mut(hwnd) }
                {
                    state
                        .app
                        .core
                        .handle(crate::core::app::Command::LibraryAddFolder(folder));
                    state.app.invalidate_all(hwnd);
                    sync_edit_boxes(state, hwnd);
                }
            }
            let pick_stream = unsafe { state_mut(hwnd) }
                .map(|state| state.app.take_stream_cache_pick())
                .unwrap_or(false);
            if pick_stream {
                if let Some(folder) = rfd::FileDialog::new().pick_folder()
                    && let Some(state) = unsafe { state_mut(hwnd) }
                {
                    state
                        .app
                        .core
                        .handle(crate::core::app::Command::SetStreamCacheDir(Some(folder)));
                    state.app.core.flush_settings();
                    state.app.invalidate_all(hwnd);
                }
            }
            let pick_download = unsafe { state_mut(hwnd) }
                .map(|state| state.app.take_download_dir_pick())
                .unwrap_or(false);
            if pick_download {
                if let Some(folder) = rfd::FileDialog::new().pick_folder()
                    && let Some(state) = unsafe { state_mut(hwnd) }
                {
                    state
                        .app
                        .core
                        .handle(crate::core::app::Command::SetDownloadDir(Some(folder)));
                    state.app.core.flush_settings();
                    state.app.invalidate_all(hwnd);
                }
            }
            let pick_lyrics_bg = unsafe { state_mut(hwnd) }
                .map(|state| state.app.take_lyrics_panel_image_pick())
                .unwrap_or(false);
            if pick_lyrics_bg {
                if let Some(file) = rfd::FileDialog::new()
                    .add_filter("图片", &["png", "jpg", "jpeg", "webp", "bmp"])
                    .pick_file()
                    && let Some(state) = unsafe { state_mut(hwnd) }
                {
                    state
                        .app
                        .core
                        .handle(crate::core::app::Command::SetLyricsPanelImage(Some(file)));
                    state.app.core.flush_settings();
                    state.app.invalidate_all(hwnd);
                }
            }
            // "打开文件" on the Now Playing card (also Ctrl+O).
            let pick_files = unsafe { state_mut(hwnd) }
                .map(|state| state.app.take_pick_files())
                .unwrap_or(false);
            if pick_files {
                pick_audio_files(hwnd);
            }
            // "选择歌词" in the lyrics panel header.
            let pick_lyrics = unsafe { state_mut(hwnd) }
                .map(|state| state.app.take_pick_lyrics())
                .unwrap_or(false);
            if pick_lyrics
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter(crate::i18n::tr("歌词"), &["lrc", "srt", "txt"])
                    .pick_file()
                && let Some(state) = unsafe { state_mut(hwnd) }
            {
                state
                    .app
                    .core
                    .handle(crate::core::app::Command::LoadLyricsFile(path));
                state.app.invalidate_all(hwnd);
                sync_lyrics(state, hwnd);
                sync_panel(state, hwnd);
            }
            unsafe { sync_timer(hwnd) };
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK => LRESULT(0),
        WM_LBUTTONUP => {
            let (color_request, bg_pick, font_pick, lyrics_font_pick, large_font_pick, theme_font_pick) =
                if let Some(state) = unsafe { state_mut(hwnd) } {
                    state.app.on_release(hwnd);
                    sync_native_brushes(state);
                    sync_lyrics(state, hwnd);
                    sync_panel(state, hwnd);
                    // A settings click while paused must reach the overlays even
                    // though the animation timer is stopped.
                    sync_overlays(state, hwnd);
                    // Hotkeys can be rebound from the settings rows.
                    sync_shortcuts(state, hwnd);
                    sync_gamepad(state, hwnd);
                    sync_tray(state);
                    // Font-size value click: show the native number editor.
                    if let Some((index, text)) = state.app.take_number_edit_request() {
                        state.app.begin_number_edit(hwnd, index);
                        set_edit_text(state.edit_number, &text);
                        // EM_SETSEL(0, -1): select all so typing replaces the value.
                        unsafe {
                            SendMessageW(
                                state.edit_number,
                                0x00B1,
                                Some(WPARAM(0)),
                                Some(LPARAM(-1)),
                            );
                        }
                        sync_inline_edits(state, hwnd);
                        let _ = unsafe { SetFocus(Some(state.edit_number)) };
                    }
                    if let Some(index) = state.app.take_text_edit_request() {
                        let text = state.app.setting_value_text(index);
                        state.app.begin_text_edit(hwnd, index);
                        set_edit_text(state.edit_text_row, &text);
                        unsafe {
                            // Place the caret at the start of long values (API
                            // URLs, JSON, templates) so the editable text is
                            // visible immediately instead of scrolled to the end.
                            SendMessageW(
                                state.edit_text_row,
                                0x00B1,
                                Some(WPARAM(0)),
                                Some(LPARAM(0)),
                            );
                        }
                        sync_inline_edits(state, hwnd);
                        let _ = unsafe { SetFocus(Some(state.edit_text_row)) };
                    }
                    (
                        state.app.take_color_request(),
                        state.app.take_desktop_bg_pick(),
                        state.app.take_desktop_font_pick(),
                        state.app.take_lyrics_font_pick(),
                        state.app.take_large_font_pick(),
                        state.app.take_theme_font_pick(),
                    )
                } else {
                    (None, false, false, false, false, false)
                };
            if let Some((field, current)) = color_request {
                // The native dialog pumps messages, so it runs after the
                // `State` borrow ends (same rule as rfd dialogs).
                let mut custom = unsafe { state_mut(hwnd) }
                    .map(|state| state.custom_colors)
                    .unwrap_or([COLORREF(0); 16]);
                if let Some(color) = choose_color(hwnd, current, &mut custom) {
                    if let Some(state) = unsafe { state_mut(hwnd) } {
                        state.custom_colors = custom;
                        state.app.apply_color(hwnd, field, color);
                        sync_lyrics(state, hwnd);
                        sync_panel(state, hwnd);
                        sync_overlays(state, hwnd);
                    }
                } else if let Some(state) = unsafe { state_mut(hwnd) } {
                    state.custom_colors = custom;
                }
            }
            if bg_pick
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter(
                        crate::i18n::tr("图片"),
                        &["png", "jpg", "jpeg", "webp", "bmp", "gif"],
                    )
                    .pick_file()
                && let Some(state) = unsafe { state_mut(hwnd) }
            {
                state
                    .app
                    .core
                    .handle(crate::core::app::Command::SetDesktopLyricsBackgroundImage(
                        Some(path),
                    ));
                state.app.core.flush_settings();
                state.app.invalidate_all(hwnd);
                sync_overlays(state, hwnd);
            }
            if font_pick {
                let current = unsafe { state_mut(hwnd) }
                    .and_then(|state| state.app.core.settings.desktop_lyrics.font_family.clone());
                if let Some(name) = choose_font(hwnd, current.as_deref()) {
                    if let Some(state) = unsafe { state_mut(hwnd) } {
                        state
                            .app
                            .core
                            .handle(crate::core::app::Command::SetDesktopLyricsFontFamily(Some(
                                name,
                            )));
                        state.app.core.flush_settings();
                        state.app.invalidate_all(hwnd);
                        sync_overlays(state, hwnd);
                    }
                }
            }
            if lyrics_font_pick {
                let current = unsafe { state_mut(hwnd) }
                    .and_then(|state| state.app.core.settings.lyrics.font_family.clone());
                if let Some(name) = choose_font(hwnd, current.as_deref()) {
                    if let Some(state) = unsafe { state_mut(hwnd) } {
                        state.app.core.handle(
                            crate::core::app::Command::SetLyricsFontFamily(Some(name)),
                        );
                        state.app.core.flush_settings();
                        state.app.invalidate_all(hwnd);
                        sync_lyrics(state, hwnd);
                        sync_overlays(state, hwnd);
                    }
                }
            }
            if large_font_pick {
                let current = unsafe { state_mut(hwnd) }
                    .and_then(|state| state.app.core.settings.large_lyrics.font_family.clone());
                if let Some(name) = choose_font(hwnd, current.as_deref()) {
                    if let Some(state) = unsafe { state_mut(hwnd) } {
                        state.app.core.handle(
                            crate::core::app::Command::SetLargeLyricsFontFamily(Some(name)),
                        );
                        state.app.core.flush_settings();
                        state.app.invalidate_all(hwnd);
                        sync_lyrics(state, hwnd);
                    }
                }
            }
            if theme_font_pick {
                let current = unsafe { state_mut(hwnd) }
                    .and_then(|state| state.app.core.settings.theme.font_family.clone());
                if let Some(name) = choose_font(hwnd, current.as_deref()) {
                    if let Some(state) = unsafe { state_mut(hwnd) } {
                        state.app.core.handle(
                            crate::core::app::Command::SetThemeFontFamily(Some(name)),
                        );
                        state.app.core.flush_settings();
                        state.app.invalidate_all(hwnd);
                    }
                }
            }
            let trans_font_pick = unsafe { state_mut(hwnd) }
                .map(|state| state.app.take_desktop_trans_font_pick())
                .unwrap_or(false);
            if trans_font_pick {
                let current = unsafe { state_mut(hwnd) }.and_then(|state| {
                    state.app.core.settings.desktop_lyrics.translation_font_family.clone()
                });
                if let Some(name) = choose_font(hwnd, current.as_deref()) {
                    if let Some(state) = unsafe { state_mut(hwnd) } {
                        state.app.core.handle(
                            crate::core::app::Command::SetDesktopLyricsTranslationFontFamily(
                                Some(name),
                            ),
                        );
                        state.app.core.flush_settings();
                        state.app.invalidate_all(hwnd);
                        sync_overlays(state, hwnd);
                    }
                }
            }
            let footer_font_pick = unsafe { state_mut(hwnd) }
                .map(|state| state.app.take_desktop_footer_font_pick())
                .unwrap_or(false);
            if footer_font_pick {
                let current = unsafe { state_mut(hwnd) }.and_then(|state| {
                    state.app.core.settings.desktop_lyrics.footer_font_family.clone()
                });
                if let Some(name) = choose_font(hwnd, current.as_deref()) {
                    if let Some(state) = unsafe { state_mut(hwnd) } {
                        state.app.core.handle(
                            crate::core::app::Command::SetDesktopLyricsFooterFontFamily(Some(
                                name,
                            )),
                        );
                        state.app.core.flush_settings();
                        state.app.invalidate_all(hwnd);
                        sync_overlays(state, hwnd);
                    }
                }
            }
            let settings_action = unsafe { state_mut(hwnd) }
                .and_then(|state| state.app.take_settings_action());
            if let Some(action) = settings_action {
                perform_settings_action(hwnd, action);
            }
            let background_pick = unsafe { state_mut(hwnd) }
                .map(|state| state.app.take_background_image_pick())
                .unwrap_or(false);
            if background_pick
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter(
                        crate::i18n::tr("图片"),
                        &["png", "jpg", "jpeg", "webp", "bmp", "gif"],
                    )
                    .pick_file()
                && let Some(state) = unsafe { state_mut(hwnd) }
            {
                state
                    .app
                    .core
                    .handle(crate::core::app::Command::SetBackgroundImage(Some(path)));
                state.app.core.flush_settings();
                state.app.invalidate_all(hwnd);
                unsafe { sync_timer(hwnd) };
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            track_mouse_leave(hwnd);
            if let Some(state) = unsafe { state_mut(hwnd) } {
                let (x, y) = message_point(lparam, state.app.dpi);
                // A stale "fully occluded" flag drops every invalidation until
                // the next poll; the mouse being inside the client proves the
                // window is exposed again, so self-heal immediately.
                if !state.app.window_visible {
                    state.app.restore_visibility(hwnd);
                }
                state.app.on_move(hwnd, x, y);
                if state.app.dragging_settings_scroll() {
                    sync_inline_edits(state, hwnd);
                }
                update_tooltip_hover(hwnd, state);
                if state.app.dragging_progress {
                    sync_lyrics(state, hwnd);
                    sync_panel(state, hwnd);
                }
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            if let Some(state) = unsafe { state_mut(hwnd) } {
                state.app.clear_hover(hwnd);
                clear_row_tooltip(hwnd, state);
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            if let Some(state) = unsafe { state_mut(hwnd) } {
                let delta = ((wparam.0 >> 16) & 0xFFFF) as u16 as i16 as f32;
                if let Some((x, y)) = cursor_dip(hwnd, state.app.dpi) {
                    if state.app.layout.sidebar.contains(x, y)
                        && state.app.sidebar_wheel(hwnd, delta)
                    {
                        return LRESULT(0);
                    }
                    let over_list = if state.app.page == crate::app::Page::Settings {
                        crate::layout::settings(&state.app.layout)
                            .generic
                            .contains(x, y)
                    } else {
                        state.app.list_area().contains(x, y)
                    };
                    state.app.on_wheel(hwnd, delta, over_list);
                    // The content moved under a stationary cursor: recompute
                    // the hovered row so its highlight and tooltip match the
                    // row now under the pointer instead of the old one.
                    state.app.on_move(hwnd, x, y);
                    sync_inline_edits(state, hwnd);
                    update_tooltip_hover(hwnd, state);
                }
            }
            LRESULT(0)
        }
        WM_HOTKEY => {
            if let Some(state) = unsafe { state_mut(hwnd) } {
                let id = wparam.0;
                if (HOTKEY_BASE..HOTKEY_BASE + SHORTCUT_COUNT).contains(&id) {
                    match id - HOTKEY_BASE {
                        0 => state.app.toggle_play(hwnd),
                        1 => state.app.next(hwnd),
                        2 => state.app.prev(hwnd),
                        3 => state.app.change_volume(hwnd, 0.05),
                        4 => state.app.change_volume(hwnd, -0.05),
                        5 => {
                            state
                                .app
                                .core
                                .handle(crate::core::app::Command::ToggleDesktopLyrics);
                            state.app.invalidate_all(hwnd);
                        }
                        6 => {
                            if unsafe { IsWindowVisible(hwnd) }.as_bool() {
                                unsafe {
                                    let _ = ShowWindow(hwnd, SW_HIDE);
                                }
                            } else {
                                unsafe {
                                    let _ = ShowWindow(hwnd, SW_SHOW);
                                    let _ = SetForegroundWindow(hwnd);
                                }
                                restore_window(state, hwnd);
                            }
                        }
                        _ => {}
                    }
                    sync_panel(state, hwnd);
                    sync_lyrics(state, hwnd);
                    sync_overlays(state, hwnd);
                    unsafe { sync_timer(hwnd) };
                }
            }
            LRESULT(0)
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            // Alt shortcuts (Alt+F4, Alt+Space) belong to the system menu
            // unless a settings row is capturing a combination.
            if message == WM_SYSKEYDOWN {
                let capturing = unsafe { state_mut(hwnd) }
                    .map(|state| state.app.shortcut_capture_active())
                    .unwrap_or(false);
                if !capturing {
                    return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
                }
            }
            let mut open_files = false;
            let mut open_editor = false;
            let mut focus_changed = false;
            let mut focus_after = None;
            if let Some(state) = unsafe { state_mut(hwnd) } {
                state.last_input = Instant::now();
                let key = wparam.0 as u16;
                if state.app.shortcut_capture_active() {
                    // The settings row owns the keyboard until a complete
                    // combination arrives (modifier-only presses are ignored).
                    if key == VK_ESCAPE.0 {
                        state.app.commit_shortcut_capture(hwnd, None);
                    } else if !crate::shortcuts::is_modifier_key(key as u32) {
                        let down = |vk: u16| unsafe { GetKeyState(vk as i32) } < 0;
                        let combo = crate::shortcuts::from_key(
                            key,
                            down(VK_CONTROL.0),
                            down(VK_MENU.0),
                            down(VK_SHIFT.0),
                            down(VK_LWIN.0) || down(VK_RWIN.0),
                        );
                        if let Some(combo) = combo {
                            state.app.commit_shortcut_capture(hwnd, Some(combo));
                        }
                    }
                    sync_shortcuts(state, hwnd);
                } else {
                    let focus_before = state.app.keyboard_focus();
                    let ctrl = unsafe { GetKeyState(0x11) } < 0; // VK_CONTROL
                    let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                    if key == 0x1B && state.app.sort_menu_open {
                        // ESC closes the native sort LISTBOX before anything else.
                        state.app.set_sort_menu_open(hwnd, false);
                        sync_edit_boxes(state, hwnd);
                    } else {
                        state.app.on_key(hwnd, key, ctrl, shift);
                    }
                    open_files = state.app.take_pick_files();
                    open_editor = state.app.take_lyrics_editor();
                    focus_changed = state.app.keyboard_focus() != focus_before;
                    focus_after = state.app.keyboard_focus();
                    sync_lyrics(state, hwnd);
                    sync_panel(state, hwnd);
                }
            }
            // Announce a Tab/arrow focus change after the `State` borrow ends;
            // the UIA tree builder also borrows the window state.
            if focus_changed
                && let Some(target) = focus_after
            {
                crate::accessibility::notify_focus(hwnd, target);
            }
            // The picker is modal and pumps messages; run it only after the
            // `State` borrow above has ended (Ctrl+O).
            if open_files {
                pick_audio_files(hwnd);
            }
            if open_editor {
                open_lyrics_editor(hwnd);
            }
            if let Some(state) = unsafe { state_mut(hwnd) } {
                sync_shortcuts(state, hwnd);
                sync_gamepad(state, hwnd);
                sync_tray(state);
            }
            unsafe { sync_timer(hwnd) };
            LRESULT(0)
        }
        WM_APP_NUMBER_COMMIT => {
            if let Some(state) = unsafe { state_mut(hwnd) } {
                let text = edit_text(state.edit_number);
                state.app.commit_number_edit(hwnd, &text);
                sync_inline_edits(state, hwnd);
                sync_lyrics(state, hwnd);
                let _ = unsafe { SetFocus(Some(hwnd)) };
            }
            unsafe { sync_timer(hwnd) };
            LRESULT(0)
        }
        WM_APP_NUMBER_CANCEL => {
            if let Some(state) = unsafe { state_mut(hwnd) } {
                state.app.cancel_number_edit(hwnd);
                sync_inline_edits(state, hwnd);
                let _ = unsafe { SetFocus(Some(hwnd)) };
            }
            LRESULT(0)
        }
        WM_APP_TEXT_COMMIT => {
            if let Some(state) = unsafe { state_mut(hwnd) } {
                let text = edit_text(state.edit_text_row);
                state.app.commit_text_edit(hwnd, &text);
                sync_inline_edits(state, hwnd);
                let _ = unsafe { SetFocus(Some(hwnd)) };
            }
            unsafe { sync_timer(hwnd) };
            LRESULT(0)
        }
        WM_APP_TEXT_CANCEL => {
            if let Some(state) = unsafe { state_mut(hwnd) } {
                state.app.cancel_text_edit(hwnd);
                sync_inline_edits(state, hwnd);
                let _ = unsafe { SetFocus(Some(hwnd)) };
            }
            LRESULT(0)
        }
        WM_APP_SEARCH_GO => {
            if let Some(state) = unsafe { state_mut(hwnd) } {
                let text = edit_text(state.edit);
                let trimmed = text.trim().to_string();
                if state.app.page == crate::app::Page::Search {
                    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
                        state
                            .app
                            .core
                            .handle(crate::core::app::Command::OpenUrl(trimmed));
                    } else {
                        state
                            .app
                            .core
                            .handle(crate::core::app::Command::OnlineSetQuery(text));
                        let limit = state.app.online_page_size() as u32;
                        state.app.core.handle(
                            crate::core::app::Command::OnlineSearch {
                                query: trimmed,
                                offset: 0,
                                limit,
                            },
                        );
                    }
                    state.app.reset_list_scroll(hwnd);
                    state.app.invalidate(hwnd, state.app.layout.header);
                    state.app
                        .invalidate(hwnd, state.app.layout.online_queue_all.inflate(2.0));
                } else {
                    state
                        .app
                        .core
                        .handle(crate::core::app::Command::LibrarySearch(text));
                    state.app.reset_list_scroll(hwnd);
                    state
                        .app
                        .core
                        .handle(crate::core::app::Command::LibraryRefresh);
                }
                sync_edit_boxes(state, hwnd);
                unsafe { sync_timer(hwnd) };
            }
            LRESULT(0)
        }
        WM_APP_FOLDER_GO => {
            if let Some(state) = unsafe { state_mut(hwnd) } {
                let text = edit_text(state.edit_folder);
                let text = text.trim().trim_matches('"').to_string();
                let target = if text.is_empty() {
                    None
                } else {
                    let path = std::path::PathBuf::from(text);
                    // Pasting a file path browses its folder instead of
                    // showing a read error for a non-directory.
                    Some(if path.is_file() {
                        path.parent().map(|parent| parent.to_path_buf()).unwrap_or(path)
                    } else {
                        path
                    })
                };
                state.app.folder_go(hwnd, target);
                let _ = unsafe { SetFocus(Some(hwnd)) };
                sync_edit_boxes(state, hwnd);
            }
            LRESULT(0)
        }
        WM_APP_FOLDER_CANCEL => {
            if let Some(state) = unsafe { state_mut(hwnd) } {
                let _ = unsafe { SetFocus(Some(hwnd)) };
                sync_edit_boxes(state, hwnd);
            }
            LRESULT(0)
        }
        crate::accessibility::WM_APP_UIA_ACTIVATE => {
            // `wparam` carries the control-center client coordinates; run the
            // normal mouse path so dialogs and editors are serviced.
            let packed = wparam.0 as isize;
            unsafe {
                let _ = PostMessageW(Some(hwnd), WM_LBUTTONDOWN, WPARAM(1), LPARAM(packed));
                let _ = PostMessageW(Some(hwnd), WM_LBUTTONUP, WPARAM(0), LPARAM(packed));
            }
            LRESULT(0)
        }
        crate::accessibility::WM_APP_UIA_FOCUS => {
            let mut focused = None;
            if let Some(state) = unsafe { state_mut(hwnd) } {
                let index = wparam.0;
                if let Some(target) = state.app.focus_targets().get(index).copied() {
                    state.app.focus_target(hwnd, target);
                    focused = Some(target);
                }
            }
            if let Some(target) = focused {
                crate::accessibility::notify_focus(hwnd, target);
            }
            LRESULT(0)
        }
        crate::accessibility::WM_APP_UIA_RANGE => {
            if let Some(state) = unsafe { state_mut(hwnd) } {
                let value = f64::from_bits(lparam.0 as u64);
                match wparam.0 {
                    1 => {
                        state.app.set_volume(value.clamp(0.0, 1.0) as f32);
                        state.app.invalidate(hwnd, state.app.layout.volume_hit);
                    }
                    2 => state.app.seek_to_secs(hwnd, value.max(0.0)),
                    _ => {}
                }
                sync_panel(state, hwnd);
                sync_lyrics(state, hwnd);
            }
            unsafe { sync_timer(hwnd) };
            LRESULT(0)
        }
        WM_APP_WAKE => {
            let forwarded = unsafe { state_mut(hwnd) }
                .map(|state| {
                    let mut args = Vec::new();
                    while let Ok(more) = state.ipc.try_recv() {
                        args.extend(more);
                    }
                    args
                })
                .unwrap_or_default();
            if let Some(state) = unsafe { state_mut(hwnd) } {
                let actions = state
                    .tray
                    .as_ref()
                    .map(|tray| tray.poll())
                    .unwrap_or_default();
                let mut repaint = false;
                for action in actions {
                    match action {
                        TrayAction::ShowWindow => {
                            unsafe {
                                let _ = ShowWindow(hwnd, SW_SHOW);
                                let _ = SetForegroundWindow(hwnd);
                            }
                            restore_window(state, hwnd);
                        }
                        TrayAction::HideWindow => unsafe {
                            let _ = ShowWindow(hwnd, SW_HIDE);
                        },
                        TrayAction::ToggleWindow => unsafe {
                            if IsWindowVisible(hwnd).as_bool() {
                                let _ = ShowWindow(hwnd, SW_HIDE);
                            } else {
                                let _ = ShowWindow(hwnd, SW_SHOW);
                                let _ = SetForegroundWindow(hwnd);
                            }
                            if IsWindowVisible(hwnd).as_bool() {
                                restore_window(state, hwnd);
                            }
                        },
                        TrayAction::PlayPause => {
                            state.app.toggle_play(hwnd);
                            repaint = true;
                        }
                        TrayAction::Prev => {
                            state.app.prev(hwnd);
                            repaint = true;
                        }
                        TrayAction::Next => {
                            state.app.next(hwnd);
                            repaint = true;
                        }
                        TrayAction::Volume(volume) => {
                            state.app.set_volume(volume.clamp(0.0, 1.0));
                        }
                        TrayAction::ToggleShuffle => {
                            state.app.core.handle(crate::core::app::Command::ToggleShuffle);
                            state.app.refresh_queue_view();
                            state.shuffle = state.app.core.snapshot().shuffle;
                            repaint = true;
                        }
                        TrayAction::CycleRepeat => {
                            state.app.core.handle(crate::core::app::Command::CycleRepeat);
                            state.repeat = state.app.core.snapshot().repeat;
                            repaint = true;
                        }
                        TrayAction::ToggleDesktopLyrics => {
                            state
                                .app
                                .core
                                .handle(crate::core::app::Command::ToggleDesktopLyrics);
                            repaint = true;
                        }
                        TrayAction::ToggleLockLyrics => {
                            let locked = state.app.core.settings.desktop_lyrics.locked;
                            state
                                .app
                                .core
                                .handle(crate::core::app::Command::DesktopLyricsSetLock(!locked));
                            repaint = true;
                        }
                        TrayAction::Quit => {
                            // The tray thread already posted WM_CLOSE; the handler
                            // below performs the fast shutdown.
                        }
                    }
                }
                // Audio/overlay threads also post WM_APP_WAKE; drain engine
                // events here so a paused window still reacts immediately.
                state.app.tick(hwnd);
                state.app.core.flush_settings();
                let _ = repaint;
                sync_tray(state);
                if state.app.window_visible {
                    sync_panel(state, hwnd);
                }
                sync_lyrics(state, hwnd);
                sync_overlays(state, hwnd);
                sync_system(state, hwnd);
                let now = Instant::now();
                state.last_overlay_sync = now;
                state.last_system_sync = now;
                unsafe { sync_timer(hwnd) };
            }
            if !forwarded.is_empty() {
                handle_forwarded_args(hwnd, &forwarded);
            }
            LRESULT(0)
        }
        WM_APP_PLAYER_ACTION => {
            if let Some(state) = unsafe { state_mut(hwnd) } {
                let (speed, eq, pitch) = {
                    let playback = &state.app.core.settings.playback;
                    (playback.speed, playback.eq_enabled, playback.pitch_preserve)
                };
                match wparam.0 {
                    ACTION_SPEED_DOWN => state
                        .app
                        .core
                        .handle(crate::core::app::Command::SetSpeed(speed - 0.05)),
                    ACTION_SPEED_UP => state
                        .app
                        .core
                        .handle(crate::core::app::Command::SetSpeed(speed + 0.05)),
                    ACTION_TOGGLE_EQ => state
                        .app
                        .core
                        .handle(crate::core::app::Command::SetEqEnabled(!eq)),
                    ACTION_TOGGLE_PITCH => state
                        .app
                        .core
                        .handle(crate::core::app::Command::SetPitchPreserve(!pitch)),
                    ACTION_OPEN_SETTINGS => {
                        state.app.set_player_panel(hwnd, false);
                        state.app.set_page(crate::app::Page::Settings);
                        state.app.invalidate_all(hwnd);
                    }
                    ACTION_SET_VOLUME => {
                        // The flyout doubles as the volume control when the
                        // player bar hides its slider on narrow windows.
                        let volume = (lparam.0 as f32 / 1000.0).clamp(0.0, 1.0);
                        state.app.set_volume(volume);
                    }
                    ACTION_SET_SLEEP => {
                        let minutes = match lparam.0 {
                            15 => Some(15),
                            30 => Some(30),
                            60 => Some(60),
                            _ => None,
                        };
                        state
                            .app
                            .core
                            .handle(crate::core::app::Command::SetSleepTimer(minutes));
                    }
                    ACTION_SLEEP_CUSTOM_DELTA => {
                        let current =
                            state.app.core.sleep_selected_minutes().unwrap_or(0) as i32;
                        let next = (current + lparam.0 as i32).clamp(0, 720);
                        state.app.core.handle(crate::core::app::Command::SetSleepTimer(
                            (next > 0).then_some(next as u32),
                        ));
                    }
                    ACTION_AB_START => state
                        .app
                        .core
                        .handle(crate::core::app::Command::SetABStart),
                    ACTION_AB_END => state
                        .app
                        .core
                        .handle(crate::core::app::Command::SetABEnd),
                    ACTION_AB_CLEAR => state
                        .app
                        .core
                        .handle(crate::core::app::Command::ClearABRepeat),
                    ACTION_DEVICE_DELTA => {
                        let current = state.app.core.settings.playback.device.clone();
                        let mut options: Vec<Option<String>> = vec![None];
                        options.extend(
                            state.app.core.output_devices().into_iter().map(Some),
                        );
                        let index = current
                            .as_ref()
                            .and_then(|device| {
                                options.iter().position(|item| item.as_ref() == Some(device))
                            })
                            .unwrap_or(0) as i32;
                        let next =
                            (index + lparam.0 as i32).rem_euclid(options.len() as i32) as usize;
                        state
                            .app
                            .core
                            .handle(crate::core::app::Command::SetOutputDevice(
                                options[next].clone(),
                            ));
                    }
                    ACTION_TOGGLE_DESKTOP_LYRICS => state
                        .app
                        .core
                        .handle(crate::core::app::Command::ToggleDesktopLyrics),
                    ACTION_TOGGLE_MINI_PLAYER => state
                        .app
                        .core
                        .handle(crate::core::app::Command::ToggleMiniPlayer),
                    _ => {}
                }
                sync_edit_boxes(state, hwnd);
                sync_panel(state, hwnd);
                sync_overlays(state, hwnd);
                if wparam.0 != ACTION_SET_VOLUME {
                    state.app.core.flush_settings();
                }
            }
            LRESULT(0)
        }
        WM_APP_LYRIC_SEEK => {
            if let Some(state) = unsafe { state_mut(hwnd) } {
                // Clicking a lyric line is an outside click for the flyout.
                state.app.set_player_panel(hwnd, false);
                let seconds = wparam.0 as f64 / 1000.0;
                state.app.seek_to_secs(hwnd, seconds);
                sync_lyrics(state, hwnd);
                sync_panel(state, hwnd);
            }
            unsafe { sync_timer(hwnd) };
            LRESULT(0)
        }
        WM_APP_LYRICS_CLOSED => {
            lyrics_window_closed(hwnd);
            unsafe { sync_timer(hwnd) };
            LRESULT(0)
        }
        WM_APP_LYRICS_SYNC => {
            if let Some(state) = unsafe { state_mut(hwnd) } {
                sync_lyrics(state, hwnd);
            }
            // Resize/restore/minimize changes whether the standalone window
            // needs animation ticks at all.
            unsafe { sync_timer(hwnd) };
            LRESULT(0)
        }
        WM_APP_EDITOR_CLOSED => {
            lyrics_editor_closed(hwnd);
            LRESULT(0)
        }
        WM_APP_EDITOR_SAVED => {
            lyrics_editor_saved(hwnd, lparam.0 as *mut std::path::PathBuf);
            unsafe { sync_timer(hwnd) };
            LRESULT(0)
        }
        WM_APP_LYRICS_ACTION => {
            if let Some(state) = unsafe { state_mut(hwnd) } {
                match wparam.0 {
                    LYRICS_ACTION_PREV => state.app.prev(hwnd),
                    LYRICS_ACTION_PLAY_PAUSE => state.app.toggle_play(hwnd),
                    LYRICS_ACTION_NEXT => state.app.next(hwnd),
                    _ => {}
                }
                sync_lyrics(state, hwnd);
                sync_panel(state, hwnd);
            }
            unsafe { sync_timer(hwnd) };
            LRESULT(0)
        }
        WM_APP_LYRIC_MENU => {
            let payload = wparam.0;
            let data = if let Some(state) = unsafe { state_mut(hwnd) } {
                lyrics_menu_data(&state.app, payload.checked_sub(1))
            } else {
                None
            };
            let outcome = data
                .map(|data| show_lyrics_menu(hwnd, data))
                .unwrap_or(LyricsMenuOutcome::Nothing);
            match outcome {
                LyricsMenuOutcome::Nothing => {}
                LyricsMenuOutcome::Reload => {
                    if let Some(state) = unsafe { state_mut(hwnd) } {
                        state
                            .app
                            .core
                            .handle(crate::core::app::Command::ReloadLyrics);
                        state.app.invalidate(hwnd, state.app.layout.lyric_header);
                        sync_lyrics(state, hwnd);
                        sync_panel(state, hwnd);
                    }
                    unsafe { sync_timer(hwnd) };
                }
                LyricsMenuOutcome::Import => {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter(crate::i18n::tr("歌词"), &["lrc", "srt", "txt"])
                        .pick_file()
                        && let Some(state) = unsafe { state_mut(hwnd) }
                    {
                        state
                            .app
                            .core
                            .handle(crate::core::app::Command::LoadLyricsFile(path));
                        state.app.invalidate_all(hwnd);
                        sync_lyrics(state, hwnd);
                        sync_panel(state, hwnd);
                    }
                }
                LyricsMenuOutcome::Export { text, name, dir } => {
                    let mut dialog = rfd::FileDialog::new()
                        .add_filter("LRC", &["lrc"])
                        .set_file_name(&name);
                    if let Some(dir) = dir {
                        dialog = dialog.set_directory(dir);
                    }
                    if let Some(mut path) = dialog.save_file() {
                        if path.extension().is_none() {
                            path.set_extension("lrc");
                        }
                        match std::fs::write(&path, text) {
                            Ok(()) => info!("lyrics exported to {}", path.display()),
                            Err(err) => warn!("export lyrics failed: {err}"),
                        }
                    }
                }
                LyricsMenuOutcome::Share(job) => {
                    let mut dialog = rfd::FileDialog::new()
                        .add_filter("PNG", &["png"])
                        .set_file_name(&job.name);
                    if let Some(dir) = job.dir.clone() {
                        dialog = dialog.set_directory(dir);
                    }
                    if let Some(mut path) = dialog.save_file() {
                        if path.extension().is_none() {
                            path.set_extension("png");
                        }
                        // GDI+ rendering + PNG encoding is deliberately off the
                        // UI thread; the card is a pure-data snapshot.
                        std::thread::spawn(move || {
                            let card = crate::util::share_image::ShareCard {
                                title: &job.title,
                                subtitle: &job.subtitle,
                                lines: &job.lines,
                                highlight: job.highlight,
                                cover: job.cover.as_deref(),
                                accent: job.accent,
                            };
                            match crate::util::share_image::render_lyrics_card(&path, &card) {
                                Ok(()) => info!("share card written to {}", path.display()),
                                Err(err) => warn!("share card failed: {err}"),
                            }
                        });
                    }
                }
            }
            LRESULT(0)
        }
        WM_CONTEXTMENU => {
            let keyboard = lparam.0 == -1;
            let mut at = POINT::default();
            if keyboard {
                let _ = unsafe { GetCursorPos(&mut at) };
            } else {
                at.x = (lparam.0 & 0xFFFF) as i16 as i32;
                at.y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            }
            let mut client = at;
            let _ = unsafe { ScreenToClient(hwnd, &mut client) };
            // Queue/library rows own the right-click menu on their pages.
            let row = if let Some(state) = unsafe { state_mut(hwnd) } {
                let scale = state.app.dpi / 96.0;
                row_menu_data(
                    &state.app,
                    client.x as f32 / scale,
                    client.y as f32 / scale,
                )
            } else {
                None
            };
            if let Some(data) = row {
                let outcome = show_row_menu(hwnd, data);
                apply_row_menu(hwnd, outcome);
                return LRESULT(0);
            }
            let menu_data = if let Some(state) = unsafe { state_mut(hwnd) } {
                let scale = state.app.dpi / 96.0;
                let (x, y) = (client.x as f32 / scale, client.y as f32 / scale);
                // Track info in the player bar, or the song card on the Now
                // Playing page. Right-clicking elsewhere keeps the default
                // behaviour (none).
                let over_info = state.app.layout.cover.contains(x, y)
                    || state.app.layout.track_text.contains(x, y)
                    || (state.app.page == crate::app::Page::NowPlaying
                        && !state.app.big_lyrics
                        && state.app.layout.card.contains(x, y));
                if over_info || keyboard {
                    state.app.current_track().map(TrackMenuData::from_track)
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(data) = menu_data {
                show_track_menu(hwnd, data, at);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            // Closing to the tray must remember the placement too, not only
            // the tray Quit path.
            remember_main_window(hwnd);
            let (quit, behavior) = unsafe { state_mut(hwnd) }
                .map(|state| {
                    let quit = state
                        .tray
                        .as_ref()
                        .map(|tray| tray.take_quit_request())
                        .unwrap_or(false);
                    (quit, state.app.core.settings.window.close_behavior)
                })
                .unwrap_or((false, crate::config::CloseBehavior::Tray));
            if quit {
                let _ = unsafe { DestroyWindow(hwnd) };
                return LRESULT(0);
            }
            match behavior {
                crate::config::CloseBehavior::Exit => {
                    let _ = unsafe { DestroyWindow(hwnd) };
                }
                crate::config::CloseBehavior::Tray => {
                    let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
                }
                crate::config::CloseBehavior::Ask => {
                    let message: Vec<u16> = "退出 Bamboo Music Player？\r\n选择“否”将最小化到托盘。"
                        .encode_utf16()
                        .chain([0])
                        .collect();
                    let answer = unsafe {
                        MessageBoxW(
                            Some(hwnd),
                            PCWSTR(message.as_ptr()),
                            w!("Bamboo Music Player"),
                            MB_YESNO | MB_ICONQUESTION,
                        )
                    };
                    if answer == IDYES {
                        let _ = unsafe { DestroyWindow(hwnd) };
                    } else {
                        let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
                    }
                }
            }
            LRESULT(0)
        }
        WM_SYSCOMMAND => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as u16;
            let code = ((wparam.0 >> 16) & 0xFFFF) as u16;
            if id as usize == ID_SORT_LIST && code as u32 == LBN_SELCHANGE {
                if let Some(state) = unsafe { state_mut(hwnd) } {
                    let index = unsafe {
                        SendMessageW(
                            state.sort_list,
                            LB_GETCURSEL,
                            Some(WPARAM(0)),
                            Some(LPARAM(0)),
                        )
                    }
                    .0;
                    if index >= 0
                        && let Some(sort) = crate::app::LIBRARY_SORTS.get(index as usize)
                    {
                        state
                            .app
                            .core
                            .handle(crate::core::app::Command::LibrarySetSort(*sort));
                        state.app.reset_list_scroll(hwnd);
                        state.app.invalidate(hwnd, state.app.layout.header);
                        state.app.invalidate(hwnd, state.app.layout.list);
                    }
                    state.app.set_sort_menu_open(hwnd, false);
                    sync_edit_boxes(state, hwnd);
                    let _ = unsafe { SetFocus(Some(hwnd)) };
                }
            }
            // EN_SETFOCUS / EN_KILLFOCUS drive the input frame highlight.
            if code == 0x0100 || code == 0x0200 {
                if let Some(state) = unsafe { state_mut(hwnd) } {
                    if code == 0x0200 && id as usize == ID_SETTING_NUMBER {
                        // Clicking elsewhere commits the typed size.
                        let text = edit_text(state.edit_number);
                        state.app.commit_number_edit(hwnd, &text);
                        sync_inline_edits(state, hwnd);
                    }
                    if code == 0x0200 && id as usize == ID_SETTING_TEXT {
                        let text = edit_text(state.edit_text_row);
                        state.app.commit_text_edit(hwnd, &text);
                        sync_inline_edits(state, hwnd);
                    }
                    let focus = if code == 0x0200 {
                        None
                    } else if id as usize == ID_SEARCH {
                        Some(crate::ui::Input::Search)
                    } else if id as usize == ID_EXTENSIONS {
                        Some(crate::ui::Input::Extensions)
                    } else if id as usize == ID_SETTING_NUMBER {
                        Some(crate::ui::Input::SettingNumber)
                    } else if id as usize == ID_SETTING_TEXT {
                        Some(crate::ui::Input::SettingNumber)
                    } else if id as usize == ID_FOLDER_PATH {
                        Some(crate::ui::Input::FolderPath)
                    } else {
                        None
                    };
                    state.app.set_input_focus(hwnd, focus);
                }
            }
            if code == EN_CHANGE {
                if let Some(state) = unsafe { state_mut(hwnd) } {
                    if id as usize == ID_SEARCH {
                        let text = edit_text(state.edit);
                        if state.app.page == crate::app::Page::Queue {
                            state.app.set_queue_filter(hwnd, text);
                        } else if state.app.page == crate::app::Page::Search {
                            // Debounced: a request per keystroke would hammer
                            // the source and discard most answers as stale.
                            state
                                .app
                                .core
                                .handle(crate::core::app::Command::OnlineSetQuery(text));
                            state.app.reset_list_scroll(hwnd);
                            // "打开链接" vs "全部入队" depends on the text.
                            state.app.invalidate(
                                hwnd,
                                state.app.layout.online_queue_all.inflate(2.0),
                            );
                            let _ = unsafe { SetTimer(Some(hwnd), SEARCH_TIMER, 350, None) };
                        } else {
                            state
                                .app
                                .core
                                .handle(crate::core::app::Command::LibrarySearch(text));
                            // New results start at the top; the old deep scroll
                            // would otherwise point past the new row set.
                            state.app.reset_list_scroll(hwnd);
                            let _ = unsafe { SetTimer(Some(hwnd), SEARCH_TIMER, 220, None) };
                        }
                    } else if id as usize == ID_EXTENSIONS {
                        let _ = unsafe { SetTimer(Some(hwnd), EXTENSIONS_TIMER, 450, None) };
                    }
                }
            }
            LRESULT(0)
        }
        WM_DRAWITEM => {
            let handle = unsafe { state_mut(hwnd) };
            if let Some(state) = handle
                && lparam.0 != 0
            {
                let item = unsafe { &*(lparam.0 as *const DRAWITEMSTRUCT) };
                if item.CtlID as usize == ID_SORT_LIST {
                    let selected = (item.itemState.0 & ODS_SELECTED.0) != 0;
                    let hovered =
                        SORT_HOVER.load(Ordering::Relaxed) == item.itemID as isize;
                    let palette = state.app.palette();
                    state.painter.set_palette(palette);
                    state.painter.draw_sort_item(
                        item.hDC,
                        item.itemID as usize,
                        selected,
                        hovered,
                        item.rcItem,
                        state.app.dpi,
                    );
                    return LRESULT(1);
                }
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        WM_CTLCOLORLISTBOX => {
            if let Some(state) = unsafe { state_mut(hwnd) } {
                let hdc = HDC(wparam.0 as *mut _);
                unsafe {
                    SetTextColor(hdc, crate::paint::edit_text_color(&state.app.palette()));
                    SetBkMode(hdc, TRANSPARENT);
                }
                return LRESULT(state.list_brush.0 as isize);
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        WM_CTLCOLOREDIT => {
            if let Some(state) = unsafe { state_mut(hwnd) }
                && (lparam.0 == state.edit.0 as isize
                    || lparam.0 == state.edit_extensions.0 as isize
                    || lparam.0 == state.edit_number.0 as isize
                    || lparam.0 == state.edit_text_row.0 as isize
                    || lparam.0 == state.edit_folder.0 as isize)
            {
                let hdc = HDC(wparam.0 as *mut _);
                unsafe {
                    SetTextColor(hdc, crate::paint::edit_text_color(&state.app.palette()));
                    SetBkMode(hdc, TRANSPARENT);
                }
                return LRESULT(state.edit_brush.0 as isize);
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        WM_DROPFILES => {
            if let Some(state) = unsafe { state_mut(hwnd) } {
                let drop = HDROP(wparam.0 as *mut _);
                let count = unsafe { DragQueryFileW(drop, u32::MAX, None) };
                let mut paths = Vec::with_capacity(count as usize);
                for index in 0..count {
                    let length = unsafe { DragQueryFileW(drop, index, None) } as usize;
                    let mut buffer = vec![0u16; length + 1];
                    let written = unsafe {
                        DragQueryFileW(drop, index, Some(buffer.as_mut_slice()))
                    } as usize;
                    paths.push(std::path::PathBuf::from(String::from_utf16_lossy(
                        &buffer[..written.min(length)],
                    )));
                }
                unsafe { DragFinish(drop) };
                if !paths.is_empty() {
                    state
                        .app
                        .core
                        .handle(crate::core::app::Command::AddPaths(paths));
                    state.app.invalidate_all(hwnd);
                    sync_lyrics(state, hwnd);
                    sync_panel(state, hwnd);
                }
            }
            unsafe { sync_timer(hwnd) };
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_DESTROY => {
            unsafe {
                let _ = RevokeDragDrop(hwnd);
            }
            let _ = unsafe { KillTimer(Some(hwnd), ANIM_TIMER) };
            let _ = unsafe { KillTimer(Some(hwnd), QUIT_TIMER) };
            let _ = unsafe { KillTimer(Some(hwnd), SEARCH_TIMER) };
            let _ = unsafe { KillTimer(Some(hwnd), EXTENSIONS_TIMER) };
            let _ = unsafe { KillTimer(Some(hwnd), TOOLTIP_TIMER) };
            let _ = unsafe { KillTimer(Some(hwnd), TOOLTIP_HIDE_TIMER) };
            let _ = unsafe { KillTimer(Some(hwnd), GAMEPAD_TIMER) };
            sync_shutdown_shortcuts(hwnd);
            if let Some(state) = unsafe { state_mut(hwnd) } {
                // `take_state` cannot read GWLP_USERDATA after the window is
                // destroyed, so AppCore::drop never runs on a normal exit.
                // Persist session/settings here while the window still exists.
                state.app.core.save_now();
                state.lyrics_editor = None;
                state.lyrics_window = None;
                let _ = unsafe { DestroyWindow(state.edit) };
                let _ = unsafe { DestroyWindow(state.edit_extensions) };
                let _ = unsafe { DestroyWindow(state.edit_number) };
                let _ = unsafe { DestroyWindow(state.sort_list) };
                if !state.tooltip.0.is_null() {
                    let _ = unsafe { DestroyWindow(state.tooltip) };
                }
                unsafe {
                    let _ = DeleteObject(HGDIOBJ(state.edit_font.0));
                    let _ = DeleteObject(HGDIOBJ(state.edit_brush.0));
                    let _ = DeleteObject(HGDIOBJ(state.list_brush.0));
                }
            }
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}
