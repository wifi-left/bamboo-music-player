//! Native lyrics editor window.
//!
//! A small MFC-style form: one multi-line `EDIT` holding the LRC document and
//! save/cancel buttons. It is deliberately not self-drawn: IME, selection,
//! clipboard and undo come from the system control, matching the project rule
//! "use a native EDIT, never a hand-painted text editor".
//!
//! The window owns no player state. `new` snapshots the current lyrics into a
//! `LyricsEditor`, `save` writes the edited text and posts the path back to the
//! main window, which reloads it through the normal `Command` path.

use std::path::PathBuf;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, EndPaint, FillRect, SetBkColor,
    SetTextColor, FONT_CHARSET, FONT_CLIP_PRECISION, FONT_OUTPUT_PRECISION, FONT_QUALITY,
    HBRUSH, HDC, HFONT, HGDIOBJ, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::SetWindowTheme;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, SetFocus, VK_CONTROL, VK_ESCAPE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect,
    GetParent, GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
    GWLP_WNDPROC, IDC_ARROW, IDYES, LoadCursorW,
    LoadIconW, MB_ICONQUESTION, MB_OK, MB_YESNO, MessageBoxW, MoveWindow, PostMessageW,
    RegisterClassW, SetWindowLongPtrW, SetWindowTextW, ShowWindow, CW_USEDEFAULT,
    GWLP_USERDATA, MINMAXINFO, SW_SHOW, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_COMMAND,
    WM_CTLCOLOREDIT, WM_ERASEBKGND, WM_GETMINMAXINFO, WM_KEYDOWN, WM_NCDESTROY, WM_PAINT,
    WM_SETFONT, WM_SIZE, WNDCLASSW, WS_CHILD, WS_CLIPCHILDREN, WS_OVERLAPPEDWINDOW,
    WS_VISIBLE, WS_VSCROLL, WNDPROC,
};
use windows::core::{PCWSTR, Result, w};

use crate::app::App;
use crate::core::lyrics::editor::LyricsEditor;

/// Posted to the player window when the editor is destroyed.
pub const WM_APP_EDITOR_CLOSED: u32 = WM_APP + 9;
/// `lparam` is a boxed `PathBuf` that the player takes ownership of and
/// reloads; 0 means the write failed.
pub const WM_APP_EDITOR_SAVED: u32 = WM_APP + 10;

const CLASS_NAME: PCWSTR = w!("BambooLyricsEditor");
const ID_EDIT: usize = 1201;
const ID_SAVE: usize = 1202;
const ID_CANCEL: usize = 1203;
const DEFAULT_WIDTH: f32 = 820.0;
const DEFAULT_HEIGHT: f32 = 600.0;
const MIN_WIDTH: f32 = 480.0;
const MIN_HEIGHT: f32 = 320.0;

/// Original EDIT window procedure, subclassed so Ctrl+S / Esc work while the
/// text control has focus (keyboard messages go to the focused child).
static EDIT_ORIG: AtomicIsize = AtomicIsize::new(0);

const C_EDIT_BG: COLORREF = rgb(22, 23, 26);
const C_TEXT: COLORREF = rgb(232, 235, 238);

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
}

struct EditorState {
    main: HWND,
    edit: HWND,
    save_button: HWND,
    cancel_button: HWND,
    font: HFONT,
    brush: HBRUSH,
    save_path: Option<PathBuf>,
    /// Text used to decide whether closing should ask for confirmation.
    initial: String,
    dirty: bool,
}

pub struct LyricsEditorWindow {
    hwnd: HWND,
}

impl LyricsEditorWindow {
    /// Create the editor from the current track/lyrics snapshot.
    pub fn new(main: HWND, app: &App) -> Result<Self> {
        let snapshot = app.core.snapshot();
        let track = snapshot.current;
        let track_key = track.map(|track| track.dedupe_key()).unwrap_or_default();
        let label = track
            .map(|track| track.display_title())
            .unwrap_or_else(|| crate::i18n::tr("无标题").to_string());
        let audio_path = track.and_then(|track| track.source.path().map(|path| path.to_path_buf()));
        let editor = LyricsEditor::from_lyrics(snapshot.lyrics, track_key, label, audio_path);
        let text = editor.to_lrc();
        let save_path = editor.default_save_path();
        let title = if track.is_some() {
            format!("歌词编辑器 · {}", editor.track_label)
        } else {
            "歌词编辑器".to_string()
        };

        unsafe {
            let instance = GetModuleHandleW(None)?;
            let icon = LoadIconW(Some(HINSTANCE(instance.0)), PCWSTR(1 as *const u16))
                .unwrap_or_default();
            let class = WNDCLASSW {
                hInstance: HINSTANCE(instance.0),
                hIcon: icon,
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                hbrBackground: HBRUSH(std::ptr::null_mut()),
                lpszClassName: CLASS_NAME,
                lpfnWndProc: Some(window_proc),
                ..Default::default()
            };
            let _ = RegisterClassW(&class);

            let dpi = GetDpiForWindow(main) as f32;
            let dpi = if dpi > 0.0 { dpi } else { 96.0 };
            let scale = dpi / 96.0;
            let width = (DEFAULT_WIDTH * scale) as i32;
            let height = (DEFAULT_HEIGHT * scale) as i32;
            let (x, y) = centered_on(main, width, height);
            let wide_title: Vec<u16> = title.encode_utf16().chain([0]).collect();
            let hwnd = CreateWindowExW(
                Default::default(),
                CLASS_NAME,
                PCWSTR(wide_title.as_ptr()),
                WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
                x,
                y,
                width,
                height,
                Some(main),
                None,
                Some(HINSTANCE(instance.0)),
                None,
            )?;

            let font = create_font(dpi);
            let edit = CreateWindowExW(
                Default::default(),
                w!("EDIT"),
                PCWSTR::null(),
                WS_CHILD
                    | WS_VISIBLE
                    | WS_VSCROLL
                    | WINDOW_STYLE(0x0004)  // ES_MULTILINE
                    | WINDOW_STYLE(0x0040)  // ES_AUTOVSCROLL
                    | WINDOW_STYLE(0x1000), // ES_WANTRETURN
                0,
                0,
                10,
                10,
                Some(hwnd),
                Some(windows::Win32::UI::WindowsAndMessaging::HMENU(
                    ID_EDIT as *mut _,
                )),
                Some(HINSTANCE(instance.0)),
                None,
            )?;
            let save_button = CreateWindowExW(
                Default::default(),
                w!("BUTTON"),
                w!("保存"),
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(0x0001_0000),
                0,
                0,
                10,
                10,
                Some(hwnd),
                Some(windows::Win32::UI::WindowsAndMessaging::HMENU(
                    ID_SAVE as *mut _,
                )),
                Some(HINSTANCE(instance.0)),
                None,
            )?;
            // BS_PUSHBUTTON is zero; WS_TABSTOP (0x10000) keeps keyboard use.
            let cancel_button = CreateWindowExW(
                Default::default(),
                w!("BUTTON"),
                w!("取消"),
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(0x0001_0000),
                0,
                0,
                10,
                10,
                Some(hwnd),
                Some(windows::Win32::UI::WindowsAndMessaging::HMENU(
                    ID_CANCEL as *mut _,
                )),
                Some(HINSTANCE(instance.0)),
                None,
            )?;

            for control in [edit, save_button, cancel_button] {
                windows::Win32::UI::WindowsAndMessaging::SendMessageW(
                    control,
                    WM_SETFONT,
                    Some(WPARAM(font.0 as usize)),
                    Some(LPARAM(1)),
                );
            }
            // 2 MB is far beyond any real LRC document.
            windows::Win32::UI::WindowsAndMessaging::SendMessageW(
                edit,
                0x00C5, // EM_SETLIMITTEXT
                Some(WPARAM(2 * 1024 * 1024)),
                Some(LPARAM(0)),
            );
            let margin = (8.0 * scale) as u32;
            let packed = margin | (margin << 16);
            windows::Win32::UI::WindowsAndMessaging::SendMessageW(
                edit,
                0x00D3, // EM_SETMARGINS
                Some(WPARAM(3)),
                Some(LPARAM(packed as isize)),
            );
            let _ = SetWindowTheme(
                edit,
                w!("DarkMode_Explorer"),
                windows::core::PCWSTR::null(),
            );
            let original =
                SetWindowLongPtrW(edit, GWLP_WNDPROC, edit_proc as *const () as isize);
            EDIT_ORIG.store(original, Ordering::Relaxed);
            set_edit_text(edit, &text);

            let state = Box::new(EditorState {
                main,
                edit,
                save_button,
                cancel_button,
                font,
                brush: CreateSolidBrush(C_EDIT_BG),
                save_path,
                initial: text,
                dirty: false,
            });
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(state) as isize);
            layout(hwnd);
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetFocus(Some(edit));
            Ok(Self { hwnd })
        }
    }
}

impl Drop for LyricsEditorWindow {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

fn create_font(dpi: f32) -> HFONT {
    let height = -((13.5 * dpi / 96.0).round() as i32);
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
            FONT_CHARSET(1),
            FONT_OUTPUT_PRECISION(0),
            FONT_CLIP_PRECISION(0),
            FONT_QUALITY(5),
            0,
            face,
        )
    }
}

fn centered_on(owner: HWND, width: i32, height: i32) -> (i32, i32) {
    let mut rect = RECT::default();
    if unsafe { GetWindowRect(owner, &mut rect) }.is_ok() {
        (
            rect.left + ((rect.right - rect.left) - width) / 2,
            rect.top + ((rect.bottom - rect.top) - height) / 2,
        )
    } else {
        (CW_USEDEFAULT, CW_USEDEFAULT)
    }
}

fn state(hwnd: HWND) -> Option<&'static EditorState> {
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const EditorState;
    unsafe { pointer.as_ref() }
}

fn state_mut(hwnd: HWND) -> Option<&'static mut EditorState> {
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut EditorState;
    unsafe { pointer.as_mut() }
}

fn layout(hwnd: HWND) {
    let Some(state) = state_mut(hwnd) else {
        return;
    };
    let dpi = unsafe { GetDpiForWindow(hwnd) } as f32;
    let scale = if dpi > 0.0 { dpi / 96.0 } else { 1.0 };
    let mut client = RECT::default();
    let _ = unsafe { GetClientRect(hwnd, &mut client) };
    let margin = (16.0 * scale).round() as i32;
    let gap = (8.0 * scale).round() as i32;
    let button_width = (88.0 * scale).round() as i32;
    let button_height = (30.0 * scale).round() as i32;
    let width = (client.right - client.left).max(1);
    let height = (client.bottom - client.top).max(1);
    let edit_height = (height - margin * 2 - button_height - gap).max(40);
    unsafe {
        let _ = MoveWindow(state.edit, margin, margin, width - margin * 2, edit_height, true);
        let save_x = width - margin - button_width * 2 - gap;
        let button_y = margin + edit_height + gap;
        let _ = MoveWindow(
            state.save_button,
            save_x,
            button_y,
            button_width,
            button_height,
            true,
        );
        let _ = MoveWindow(
            state.cancel_button,
            save_x + button_width + gap,
            button_y,
            button_width,
            button_height,
            true,
        );
    }
}

/// A Win32 multiline EDIT only treats CRLF as a line break; LRC text uses LF.
/// Normalize both directions so the document keeps LF on disk/parse but still
/// displays one line per lyric line.
fn set_edit_text(edit: HWND, text: &str) {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let crlf = normalized.replace('\n', "\r\n");
    let wide: Vec<u16> = crlf.encode_utf16().chain([0]).collect();
    let _ = unsafe { SetWindowTextW(edit, PCWSTR(wide.as_ptr())) };
}

fn edit_text(edit: HWND) -> String {
    let length = unsafe { GetWindowTextLengthW(edit) }.max(0) as usize;
    let mut buffer = vec![0u16; length + 1];
    let written = unsafe { GetWindowTextW(edit, &mut buffer) }.max(0) as usize;
    String::from_utf16_lossy(&buffer[..written.min(buffer.len())])
        .replace("\r\n", "\n")
        .replace('\r', "\n")
}

/// Save the current text. Returns true when the editor should close.
unsafe fn save(hwnd: HWND) -> bool {
    let Some((text, save_path, main)) = state(hwnd).map(|state| {
        (
            edit_text(state.edit),
            state.save_path.clone(),
            state.main,
        )
    }) else {
        return false;
    };
    let path = match save_path {
        Some(path) => path,
        None => {
            // rfd runs its own message loop; no `EditorState` borrow may be
            // held across it.
            let dialog = rfd::FileDialog::new()
                .add_filter("LRC", &["lrc", "txt"])
                .set_file_name("lyrics.lrc");
            match dialog.save_file() {
                Some(path) => path,
                None => return false,
            }
        }
    };
    if let Err(err) = std::fs::write(&path, text.as_bytes()) {
        let message: Vec<u16> = format!("保存失败：{err}").encode_utf16().chain([0]).collect();
        let title: Vec<u16> = "歌词编辑器".encode_utf16().chain([0]).collect();
        unsafe {
            MessageBoxW(
                Some(hwnd),
                PCWSTR(message.as_ptr()),
                PCWSTR(title.as_ptr()),
                MB_OK | MB_ICONQUESTION,
            );
        }
        return false;
    }
    if let Some(state) = state_mut(hwnd) {
        state.save_path = Some(path.clone());
        state.initial = text;
        state.dirty = false;
    }
    let path = Box::into_raw(Box::new(path));
    unsafe {
        let _ = PostMessageW(
            Some(main),
            WM_APP_EDITOR_SAVED,
            WPARAM(0),
            LPARAM(path as isize),
        );
    }
    true
}

unsafe fn close_editor(hwnd: HWND) {
    if let Some(state) = state(hwnd)
        && state.dirty
    {
        let message: Vec<u16> = "放弃未保存的修改？"
            .encode_utf16()
            .chain([0])
            .collect();
        let title: Vec<u16> = "歌词编辑器".encode_utf16().chain([0]).collect();
        let answer = unsafe {
            MessageBoxW(
                Some(hwnd),
                PCWSTR(message.as_ptr()),
                PCWSTR(title.as_ptr()),
                MB_YESNO | MB_ICONQUESTION,
            )
        };
        if answer != IDYES {
            return;
        }
    }
    let main = state(hwnd).map(|state| state.main);
    if let Some(main) = main {
        unsafe {
            let _ =
                PostMessageW(Some(main), WM_APP_EDITOR_CLOSED, WPARAM(0), LPARAM(0));
        }
    }
    let _ = unsafe { DestroyWindow(hwnd) };
}

unsafe extern "system" fn edit_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_KEYDOWN {
        let key = wparam.0 as u16;
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
        if ctrl && key == b'S' as u16 {
            if let Ok(parent) = unsafe { GetParent(hwnd) } {
                let _ = unsafe { PostMessageW(Some(parent), WM_COMMAND, WPARAM(ID_SAVE), LPARAM(0)) };
            }
            return LRESULT(0);
        }
        if key == VK_ESCAPE.0 {
            if let Ok(parent) = unsafe { GetParent(hwnd) } {
                let _ = unsafe {
                    PostMessageW(Some(parent), WM_COMMAND, WPARAM(ID_CANCEL), LPARAM(0))
                };
            }
            return LRESULT(0);
        }
    }
    let original = EDIT_ORIG.load(Ordering::Relaxed);
    if original == 0 {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    let procedure: WNDPROC = unsafe { std::mem::transmute(original) };
    unsafe { CallWindowProcW(procedure, hwnd, message, wparam, lparam) }
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
            let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
            if let Some(state) = state(hwnd) {
                let mut rect = RECT::default();
                let _ = unsafe { GetClientRect(hwnd, &mut rect) };
                unsafe {
                    FillRect(hdc, &rect, state.brush);
                }
            }
            let _ = unsafe { EndPaint(hwnd, &ps) };
            LRESULT(0)
        }
        WM_ERASEBKGND => {
            if let Some(state) = state(hwnd) {
                let mut rect = RECT::default();
                let _ = unsafe { GetClientRect(hwnd, &mut rect) };
                unsafe {
                    FillRect(HDC(wparam.0 as *mut _), &rect, state.brush);
                }
            }
            LRESULT(1)
        }
        WM_SIZE => {
            layout(hwnd);
            LRESULT(0)
        }
        WM_GETMINMAXINFO => {
            if lparam.0 != 0 {
                let dpi = unsafe { GetDpiForWindow(hwnd) } as f32;
                let scale = if dpi > 0.0 { dpi / 96.0 } else { 1.0 };
                let info = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
                info.ptMinTrackSize.x = (MIN_WIDTH * scale).round() as i32;
                info.ptMinTrackSize.y = (MIN_HEIGHT * scale).round() as i32;
            }
            LRESULT(0)
        }
        WM_CTLCOLOREDIT => {
            if let Some(state) = state(hwnd) {
                let hdc = HDC(wparam.0 as *mut _);
                unsafe {
                    SetTextColor(hdc, C_TEXT);
                    SetBkColor(hdc, C_EDIT_BG);
                }
                return LRESULT(state.brush.0 as isize);
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = wparam.0 & 0xFFFF;
            let notification = ((wparam.0 >> 16) & 0xFFFF) as u32;
            match id {
                ID_SAVE => {
                    if unsafe { save(hwnd) } {
                        unsafe { close_editor(hwnd) };
                    }
                    return LRESULT(0);
                }
                ID_CANCEL => {
                    unsafe { close_editor(hwnd) };
                    return LRESULT(0);
                }
                ID_EDIT if notification == 0x0300 => {
                    // EN_CHANGE: track dirtiness for the close prompt.
                    if let Some(state) = state_mut(hwnd) {
                        state.dirty = edit_text(state.edit) != state.initial;
                    }
                    return LRESULT(0);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let key = wparam.0 as u16;
            let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
            if ctrl && key == b'S' as u16 {
                if unsafe { save(hwnd) } {
                    unsafe { close_editor(hwnd) };
                }
            } else if key == VK_ESCAPE.0 {
                unsafe { close_editor(hwnd) };
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            unsafe { close_editor(hwnd) };
            LRESULT(0)
        }
        WM_NCDESTROY => {
            let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut EditorState;
            if !pointer.is_null() {
                let state = unsafe { Box::from_raw(pointer) };
                unsafe {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                    let _ = DeleteObject(HGDIOBJ(state.font.0));
                    let _ = DeleteObject(HGDIOBJ(state.brush.0));
                }
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}
