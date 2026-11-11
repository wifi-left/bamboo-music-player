//! UI Automation provider for the custom-drawn main window.
//!
//! The player paints every control with GDI, so Windows cannot discover them
//! on its own. This module exposes the same focus order used by Tab as a UIA
//! fragment tree: the window is the fragment root, each control is a child
//! with a name, control type, screen rectangle and (for sliders) a range
//! pattern. Invoke/focus/range calls post messages to the UI thread, so the
//! provider never touches player state from a UIA worker thread.

use std::ffi::c_void;
use std::sync::Arc;

use windows::Win32::Foundation::{E_NOTIMPL, HWND, LPARAM, POINT, RECT, VARIANT_BOOL, WPARAM};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::System::Variant::{VARIANT, VT_BOOL, VT_BSTR, VT_I4};
use windows::Win32::UI::Accessibility::{
    IInvokeProvider, IInvokeProvider_Impl, IRangeValueProvider, IRangeValueProvider_Impl,
    IRawElementProviderFragment, IRawElementProviderFragment_Impl, IRawElementProviderFragmentRoot,
    IRawElementProviderFragmentRoot_Impl, IRawElementProviderSimple,
    IRawElementProviderSimple_Impl, NavigateDirection, NavigateDirection_FirstChild,
    NavigateDirection_LastChild, NavigateDirection_NextSibling, NavigateDirection_Parent,
    NavigateDirection_PreviousSibling, ProviderOptions, ProviderOptions_ServerSideProvider,
    UIA_AutomationFocusChangedEventId, UIA_ButtonControlTypeId, UIA_ControlTypePropertyId,
    UIA_InvokePatternId, UIA_IsEnabledPropertyId, UIA_IsKeyboardFocusablePropertyId,
    UIA_IsOffscreenPropertyId, UIA_NamePropertyId, UIA_RangeValuePatternId,
    UIA_SliderControlTypeId, UIA_TextControlTypeId, UIA_ValueValuePropertyId,
    UIA_WindowControlTypeId,
    UiaHostProviderFromHwnd, UiaRaiseAutomationEvent, UiaRect, UiaReturnRawElementProvider,
    UiaRootObjectId,
};
use windows::Win32::UI::WindowsAndMessaging::{GetWindowRect, PostMessageW, WM_APP};
use windows::core::{BOOL, BSTR, Error, IUnknown, Interface, Result};

use crate::app::Page;
use crate::ui::Target;

/// Activate a control: `wparam` carries packed client coordinates.
pub const WM_APP_UIA_ACTIVATE: u32 = WM_APP + 21;
/// Move the keyboard focus to the control at index `wparam`.
pub const WM_APP_UIA_FOCUS: u32 = WM_APP + 22;
/// Set a slider: `wparam` 1 = volume, 2 = seek; `lparam` = f64 bits.
pub const WM_APP_UIA_RANGE: u32 = WM_APP + 23;

#[cfg(test)]
const CONTROL_TYPE_BUTTON: i32 = 50000;
#[cfg(test)]
const CONTROL_TYPE_SLIDER: i32 = 50015;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Slider {
    Volume,
    Progress,
}

/// What kind of UIA node a generic overlay element is.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum UiType {
    Button,
    Slider,
    Text,
}

/// One element published by an overlay window. Coordinates are screen pixels;
/// `click` is the packed client-pixel center used by the Invoke pattern.
pub struct UiElement {
    pub name: String,
    pub kind: UiType,
    pub rect: UiaRect,
    pub click: isize,
    pub value: f64,
    pub minimum: f64,
    pub maximum: f64,
    pub step: f64,
    pub page: f64,
    pub read_only: bool,
    pub value_text: String,
}

impl UiElement {
    pub fn button(name: impl Into<String>, rect: UiaRect, click: isize) -> Self {
        Self {
            name: name.into(),
            kind: UiType::Button,
            rect,
            click,
            value: 0.0,
            minimum: 0.0,
            maximum: 0.0,
            step: 0.0,
            page: 0.0,
            read_only: true,
            value_text: String::new(),
        }
    }

    pub fn text(name: impl Into<String>, rect: UiaRect) -> Self {
        Self {
            name: name.into(),
            kind: UiType::Text,
            rect,
            click: 0,
            value: 0.0,
            minimum: 0.0,
            maximum: 0.0,
            step: 0.0,
            page: 0.0,
            read_only: true,
            value_text: String::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn slider(
        name: impl Into<String>,
        rect: UiaRect,
        click: isize,
        value: f64,
        minimum: f64,
        maximum: f64,
        step: f64,
        page: f64,
        read_only: bool,
        value_text: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            kind: UiType::Slider,
            rect,
            click,
            value,
            minimum,
            maximum,
            step,
            page,
            read_only,
            value_text: value_text.into(),
        }
    }
}

/// Build a provider for an overlay window from a static element list. The
/// overlay handles `WM_APP_UIA_ACTIVATE`/`WM_APP_UIA_RANGE` itself.
pub fn publish(hwnd: HWND, elements: Vec<UiElement>) -> IRawElementProviderSimple {
    let controls = elements
        .into_iter()
        .map(|element| Control {
            target: None,
            name: element.name,
            slider: (element.kind == UiType::Slider).then_some(Slider::Progress),
            kind: element.kind,
            rect: element.rect,
            click: element.click,
            value: element.value,
            minimum: element.minimum,
            maximum: element.maximum,
            step: element.step,
            page: element.page,
            read_only: element.read_only,
            value_text: element.value_text,
        })
        .collect();
    RootProvider {
        hwnd: hwnd.0 as isize,
        tree: Arc::new(Tree {
            root_name: window_title(hwnd),
            controls,
            focused: None,
        }),
    }
    .into()
}

/// `WM_GETOBJECT` helper for overlay windows.
pub fn return_published(
    hwnd: HWND,
    wparam: WPARAM,
    lparam: LPARAM,
    provider: &IRawElementProviderSimple,
) -> windows::Win32::Foundation::LRESULT {
    unsafe { UiaReturnRawElementProvider(hwnd, wparam, lparam, provider) }
}

#[derive(Clone)]
struct Control {
    target: Option<Target>,
    name: String,
    slider: Option<Slider>,
    kind: UiType,
    /// Screen coordinates in physical pixels.
    rect: UiaRect,
    /// Packed client coordinates (physical pixels) of the control center.
    click: isize,
    value: f64,
    minimum: f64,
    maximum: f64,
    step: f64,
    page: f64,
    read_only: bool,
    value_text: String,
}

struct Tree {
    root_name: String,
    controls: Vec<Control>,
    focused: Option<usize>,
}

/// Build the tree from the current player state. Called on the UI thread when
/// Windows asks for the root object.
pub fn root(hwnd: HWND) -> IRawElementProviderSimple {
    RootProvider {
        hwnd: hwnd.0 as isize,
        tree: build_tree(hwnd),
    }
    .into()
}

fn build_tree(hwnd: HWND) -> Arc<Tree> {
    let mut controls = Vec::new();
    let mut focused = None;
    let Some(state) = (unsafe { crate::state_mut(hwnd) }) else {
        return Arc::new(Tree {
            root_name: "Bamboo Music Player".to_string(),
            controls,
            focused,
        });
    };
    let scale = state.app.dpi / 96.0;
    let mut origin = POINT::default();
    let _ = unsafe { ClientToScreen(hwnd, &mut origin) };
    let snapshot = state.app.core.snapshot();
    for target in state.app.focus_targets() {
        let client = state.app.target_rect(target);
        if client.width() <= 0.0 || client.height() <= 0.0 {
            continue;
        }
        let slider = match target {
            Target::Volume => Some(Slider::Volume),
            Target::Progress => Some(Slider::Progress),
            _ => None,
        };
        let (value, minimum, maximum, step, page, read_only, value_text) = match slider {
            Some(Slider::Volume) => (
                snapshot.volume as f64,
                0.0,
                1.0,
                0.05,
                0.1,
                false,
                format!("{:.0}%", snapshot.volume * 100.0),
            ),
            Some(Slider::Progress) => {
                let duration = snapshot.duration_secs;
                let seek = state.app.core.settings.playback.seek_step_secs;
                (
                    snapshot.position_secs.min(duration.max(0.0)),
                    0.0,
                    duration.max(0.0),
                    seek,
                    10.0,
                    duration <= 0.5,
                    format!(
                        "{} / {}",
                        format_time(snapshot.position_secs),
                        format_time(duration)
                    ),
                )
            }
            None => (0.0, 0.0, 0.0, 0.0, 0.0, false, String::new()),
        };
        let rect = UiaRect {
            left: origin.x as f64 + client.left as f64 * scale as f64,
            top: origin.y as f64 + client.top as f64 * scale as f64,
            width: (client.right - client.left) as f64 * scale as f64,
            height: (client.bottom - client.top) as f64 * scale as f64,
        };
        let cx = ((client.left + client.right) * 0.5 * scale).round() as i32;
        let cy = ((client.top + client.bottom) * 0.5 * scale).round() as i32;
        let click = (((cy as u16 as u32) << 16) | (cx as u16 as u32)) as isize;
        controls.push(Control {
            target: Some(target),
            name: control_name(&state.app, target),
            slider,
            kind: if slider.is_some() {
                UiType::Slider
            } else {
                UiType::Button
            },
            rect,
            click,
            value,
            minimum,
            maximum,
            step,
            page,
            read_only,
            value_text,
        });
    }
    if let Some(focus) = state.app.keyboard_focus() {
        focused = controls
            .iter()
            .position(|control| control.target == Some(focus));
    }
    Arc::new(Tree {
        root_name: "Bamboo Music Player".to_string(),
        controls,
        focused,
    })
}

fn control_name(app: &crate::app::App, target: Target) -> String {
    let snapshot = app.core.snapshot();
    let page = |index: usize| Page::ALL.get(index).map(|page| page.label()).unwrap_or("");
    match target {
        Target::Nav(index) => crate::i18n::format_template("页面：{}", &[&page(index)]),
        Target::Shuffle => crate::i18n::format_template(
            "随机播放：{}",
            &[&crate::i18n::tr(if snapshot.shuffle { "开" } else { "关" })],
        ),
        Target::Repeat => crate::i18n::format_template(
            "循环模式：{}",
            &[&crate::i18n::tr(match snapshot.repeat {
                crate::core::model::RepeatMode::Off => "关闭",
                crate::core::model::RepeatMode::All => "列表循环",
                crate::core::model::RepeatMode::One => "单曲循环",
            })],
        ),
        Target::Prev => crate::i18n::tr("上一首").to_string(),
        Target::Play => crate::i18n::tr(if app.is_playing() { "暂停" } else { "播放" }).to_string(),
        Target::Next => crate::i18n::tr("下一首").to_string(),
        Target::Volume => crate::i18n::format_template(
            "音量 {:.0}%",
            &[&(snapshot.volume * 100.0).round()],
        ),
        Target::Mute => crate::i18n::tr(if snapshot.muted {
            "取消静音"
        } else {
            "静音"
        })
        .to_string(),
        Target::PlaySettings => crate::i18n::tr("播放设置").to_string(),
        Target::Playlist => crate::i18n::tr("播放列表").to_string(),
        Target::MiniPlayer => crate::i18n::tr("迷你播放器").to_string(),
        Target::DesktopLyrics => crate::i18n::tr("桌面歌词").to_string(),
        Target::Progress => crate::i18n::format_template(
            "播放进度 {} / {}",
            &[
                &format_time(snapshot.position_secs),
                &format_time(snapshot.duration_secs),
            ],
        ),
        Target::QueueImport => crate::i18n::tr("导入歌单").to_string(),
        Target::QueueExport => crate::i18n::tr("导出歌单").to_string(),
        Target::QueueClear => crate::i18n::tr("清空队列").to_string(),
        Target::Sort => crate::i18n::tr("排序方式").to_string(),
        Target::AddFolder => crate::i18n::tr("添加目录").to_string(),
        Target::Rescan => crate::i18n::tr("重新扫描曲库").to_string(),
        Target::LibraryQueueAll => crate::i18n::tr("全部加入队列").to_string(),
        Target::LibraryCheckMissing => crate::i18n::tr("检查缺失文件").to_string(),
        Target::LibraryPrev => crate::i18n::tr("上一页").to_string(),
        Target::LibraryNext => crate::i18n::tr("下一页").to_string(),
        Target::FolderBack => crate::i18n::tr("后退").to_string(),
        Target::FolderForward => crate::i18n::tr("前进").to_string(),
        Target::FolderUp => crate::i18n::tr("上一级目录").to_string(),
        Target::FolderRefresh => crate::i18n::tr("刷新目录").to_string(),
        Target::FolderPlayAll => crate::i18n::tr("播放全部").to_string(),
        Target::FolderQueueAll => crate::i18n::tr("全部加入队列").to_string(),
        Target::OnlineKind => crate::i18n::tr("搜索类型").to_string(),
        Target::OnlineBack => crate::i18n::tr("返回").to_string(),
        Target::OnlineQueueAll => crate::i18n::tr("全部加入队列").to_string(),
        Target::OnlineRandom => crate::i18n::tr("随机推荐").to_string(),
        Target::OnlineWall => crate::i18n::tr("封面墙").to_string(),
        Target::OnlinePrev => crate::i18n::tr("上一页").to_string(),
        Target::OnlineNext => crate::i18n::tr("下一页").to_string(),
        Target::DiscoverRefresh => crate::i18n::tr("换一批").to_string(),
        Target::StatsRefresh => crate::i18n::tr("刷新统计").to_string(),
        Target::StatsReset => crate::i18n::tr("重置统计").to_string(),
        Target::DownloadsCleanPart => crate::i18n::tr("清理未完成文件").to_string(),
        Target::DownloadsClearFinished => crate::i18n::tr("清除已完成任务").to_string(),
        Target::SmartKind => crate::i18n::tr("智能歌单类型").to_string(),
        Target::DuplicateMode => crate::i18n::tr("重复判定方式").to_string(),
        Target::SettingsTab(index) => crate::i18n::format_template(
            "设置分类：{}",
            &[&crate::app::SettingsTab::ALL
                .get(index)
                .map(|tab| tab.label())
                .unwrap_or("")],
        ),
        Target::CardPlay => crate::i18n::tr("播放 / 暂停").to_string(),
        Target::CardOpen => crate::i18n::tr("打开文件").to_string(),
        Target::LyricsPick => crate::i18n::tr("选择歌词文件").to_string(),
        Target::LyricsWindow => crate::i18n::tr("独立歌词窗口").to_string(),
        Target::LyricsBig => crate::i18n::tr("全屏歌词").to_string(),
        Target::LyricsEdit => crate::i18n::tr("编辑歌词").to_string(),
        Target::LyricsReload => crate::i18n::tr("重新加载歌词").to_string(),
        Target::SettingsRow(index) => app
            .settings_views()
            .get(index)
            .map(|view| {
                let label = crate::i18n::tr(view.label);
                if view.value.trim().is_empty() {
                    label.to_string()
                } else {
                    crate::i18n::format_template("{}：{}", &[&label, &view.value])
                }
            })
            .unwrap_or_default(),
        Target::StatsRecent(index) => snapshot
            .statistics
            .and_then(|stats| stats.recently_played.get(index))
            .map(|entry| {
                if entry.artist.trim().is_empty() {
                    entry.title.clone()
                } else {
                    format!("{} - {}", entry.title, entry.artist)
                }
            })
            .unwrap_or_default(),
        Target::Row(index) => app
            .row_info(index)
            .map(|row| {
                if row.artist.trim().is_empty() {
                    row.title.to_string()
                } else {
                    format!("{} - {}", row.title, row.artist)
                }
            })
            .unwrap_or_default(),
        _ => format!("{target:?}"),
    }
}

fn window_title(hwnd: HWND) -> String {
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowTextLengthW, GetWindowTextW};
    let length = unsafe { GetWindowTextLengthW(hwnd) };
    if length <= 0 {
        return "Bamboo Music Player".to_string();
    }
    let mut buffer = vec![0u16; length as usize + 1];
    let copied = unsafe { GetWindowTextW(hwnd, &mut buffer) };
    String::from_utf16_lossy(&buffer[..copied as usize])
}

fn format_time(seconds: f64) -> String {
    let seconds = seconds.max(0.0) as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn variant_bstr(text: &str) -> VARIANT {
    let mut value = VARIANT::default();
    unsafe {
        let inner = &mut value.Anonymous.Anonymous;
        inner.vt = VT_BSTR;
        inner.Anonymous.bstrVal = core::mem::ManuallyDrop::new(BSTR::from(text));
    }
    value
}

fn variant_i4(number: i32) -> VARIANT {
    let mut value = VARIANT::default();
    unsafe {
        let inner = &mut value.Anonymous.Anonymous;
        inner.vt = VT_I4;
        inner.Anonymous.lVal = number;
    }
    value
}

fn variant_bool(on: bool) -> VARIANT {
    let mut value = VARIANT::default();
    unsafe {
        let inner = &mut value.Anonymous.Anonymous;
        inner.vt = VT_BOOL;
        inner.Anonymous.boolVal = VARIANT_BOOL(if on { -1 } else { 0 });
    }
    value
}

fn unavailable() -> Error {
    Error::from_hresult(E_NOTIMPL)
}

fn window_rect(hwnd: isize) -> UiaRect {
    let mut rect = RECT::default();
    if unsafe { GetWindowRect(HWND(hwnd as *mut c_void), &mut rect) }.is_err() {
        return UiaRect::default();
    }
    UiaRect {
        left: rect.left as f64,
        top: rect.top as f64,
        width: (rect.right - rect.left) as f64,
        height: (rect.bottom - rect.top) as f64,
    }
}

#[allow(non_snake_case)]
#[windows::core::implement(
    IRawElementProviderSimple,
    IRawElementProviderFragment,
    IRawElementProviderFragmentRoot
)]
struct RootProvider {
    hwnd: isize,
    tree: Arc<Tree>,
}

#[allow(non_snake_case)]
impl IRawElementProviderSimple_Impl for RootProvider_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider)
    }

    fn GetPatternProvider(
        &self,
        _patternid: windows::Win32::UI::Accessibility::UIA_PATTERN_ID,
    ) -> Result<IUnknown> {
        Err(unavailable())
    }

    fn GetPropertyValue(
        &self,
        propertyid: windows::Win32::UI::Accessibility::UIA_PROPERTY_ID,
    ) -> Result<VARIANT> {
        let id = propertyid.0;
        if id == UIA_NamePropertyId.0 {
            return Ok(variant_bstr(&self.tree.root_name));
        }
        if id == UIA_ControlTypePropertyId.0 {
            return Ok(variant_i4(UIA_WindowControlTypeId.0));
        }
        if id == UIA_IsEnabledPropertyId.0 {
            return Ok(variant_bool(true));
        }
        if id == UIA_IsKeyboardFocusablePropertyId.0 {
            return Ok(variant_bool(false));
        }
        if id == UIA_IsOffscreenPropertyId.0 {
            return Ok(variant_bool(false));
        }
        Ok(VARIANT::default())
    }

    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        unsafe { UiaHostProviderFromHwnd(HWND(self.hwnd as *mut c_void)) }
    }
}

#[allow(non_snake_case)]
impl IRawElementProviderFragment_Impl for RootProvider_Impl {
    fn Navigate(&self, direction: NavigateDirection) -> Result<IRawElementProviderFragment> {
        if direction == NavigateDirection_FirstChild && !self.tree.controls.is_empty() {
            return Ok(child(self.hwnd, self.tree.clone(), 0));
        }
        if direction == NavigateDirection_LastChild && !self.tree.controls.is_empty() {
            return Ok(child(
                self.hwnd,
                self.tree.clone(),
                self.tree.controls.len() - 1,
            ));
        }
        Err(unavailable())
    }

    fn GetRuntimeId(&self) -> Result<*mut windows::Win32::System::Com::SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }

    fn BoundingRectangle(&self) -> Result<UiaRect> {
        Ok(window_rect(self.hwnd))
    }

    fn GetEmbeddedFragmentRoots(&self) -> Result<*mut windows::Win32::System::Com::SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }

    fn SetFocus(&self) -> Result<()> {
        Ok(())
    }

    fn FragmentRoot(&self) -> Result<IRawElementProviderFragmentRoot> {
        Ok(RootProvider {
            hwnd: self.hwnd,
            tree: self.tree.clone(),
        }
        .into())
    }
}

#[allow(non_snake_case)]
impl IRawElementProviderFragmentRoot_Impl for RootProvider_Impl {
    fn ElementProviderFromPoint(&self, x: f64, y: f64) -> Result<IRawElementProviderFragment> {
        for (index, control) in self.tree.controls.iter().enumerate() {
            let rect = control.rect;
            if x >= rect.left
                && x < rect.left + rect.width
                && y >= rect.top
                && y < rect.top + rect.height
            {
                return Ok(child(self.hwnd, self.tree.clone(), index));
            }
        }
        Ok(self.clone_root())
    }

    fn GetFocus(&self) -> Result<IRawElementProviderFragment> {
        if let Some(index) = self.tree.focused {
            return Ok(child(self.hwnd, self.tree.clone(), index));
        }
        if self.tree.controls.is_empty() {
            return Ok(self.clone_root());
        }
        Ok(child(self.hwnd, self.tree.clone(), 0))
    }
}

impl RootProvider {
    fn clone_root(&self) -> IRawElementProviderFragment {
        RootProvider {
            hwnd: self.hwnd,
            tree: self.tree.clone(),
        }
        .into()
    }
}

#[allow(non_snake_case)]
#[windows::core::implement(IRawElementProviderSimple, IRawElementProviderFragment)]
struct ControlProvider {
    hwnd: isize,
    tree: Arc<Tree>,
    index: usize,
}

fn child(hwnd: isize, tree: Arc<Tree>, index: usize) -> IRawElementProviderFragment {
    ControlProvider { hwnd, tree, index }.into()
}

#[allow(non_snake_case)]
impl IRawElementProviderSimple_Impl for ControlProvider_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider)
    }

    fn GetPatternProvider(
        &self,
        patternid: windows::Win32::UI::Accessibility::UIA_PATTERN_ID,
    ) -> Result<IUnknown> {
        let Some(control) = self.tree.controls.get(self.index) else {
            return Err(unavailable());
        };
        if control.kind == UiType::Slider && patternid == UIA_RangeValuePatternId {
            let pattern: IRangeValueProvider = RangePattern {
                hwnd: self.hwnd,
                slider: control.slider.unwrap(),
                value: control.value,
                minimum: control.minimum,
                maximum: control.maximum,
                step: control.step,
                page: control.page,
                read_only: control.read_only,
            }
            .into();
            return pattern.cast();
        }
        if control.kind == UiType::Button && patternid == UIA_InvokePatternId {
            let pattern: IInvokeProvider = InvokePattern {
                hwnd: self.hwnd,
                packed: control.click,
            }
            .into();
            return pattern.cast();
        }
        Err(unavailable())
    }

    fn GetPropertyValue(
        &self,
        propertyid: windows::Win32::UI::Accessibility::UIA_PROPERTY_ID,
    ) -> Result<VARIANT> {
        let Some(control) = self.tree.controls.get(self.index) else {
            return Ok(VARIANT::default());
        };
        let id = propertyid.0;
        if id == UIA_NamePropertyId.0 {
            return Ok(variant_bstr(&control.name));
        }
        if id == UIA_ControlTypePropertyId.0 {
            return Ok(variant_i4(match control.kind {
                UiType::Slider => UIA_SliderControlTypeId.0,
                UiType::Button => UIA_ButtonControlTypeId.0,
                UiType::Text => UIA_TextControlTypeId.0,
            }));
        }
        if id == UIA_IsEnabledPropertyId.0 {
            return Ok(variant_bool(true));
        }
        if id == UIA_IsKeyboardFocusablePropertyId.0 {
            return Ok(variant_bool(true));
        }
        if id == UIA_IsOffscreenPropertyId.0 {
            return Ok(variant_bool(false));
        }
        if id == UIA_ValueValuePropertyId.0 && control.slider.is_some() {
            return Ok(variant_bstr(&control.value_text));
        }
        Ok(VARIANT::default())
    }

    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        Err(unavailable())
    }
}

#[allow(non_snake_case)]
impl IRawElementProviderFragment_Impl for ControlProvider_Impl {
    fn Navigate(&self, direction: NavigateDirection) -> Result<IRawElementProviderFragment> {
        if direction == NavigateDirection_Parent {
            return Ok(RootProvider {
                hwnd: self.hwnd,
                tree: self.tree.clone(),
            }
            .into());
        }
        if direction == NavigateDirection_NextSibling {
            let next = self.index + 1;
            if next < self.tree.controls.len() {
                return Ok(child(self.hwnd, self.tree.clone(), next));
            }
        }
        if direction == NavigateDirection_PreviousSibling
            && let Some(previous) = self.index.checked_sub(1)
        {
            return Ok(child(self.hwnd, self.tree.clone(), previous));
        }
        Err(unavailable())
    }

    fn GetRuntimeId(&self) -> Result<*mut windows::Win32::System::Com::SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }

    fn BoundingRectangle(&self) -> Result<UiaRect> {
        Ok(self
            .tree
            .controls
            .get(self.index)
            .map(|control| control.rect)
            .unwrap_or_default())
    }

    fn GetEmbeddedFragmentRoots(&self) -> Result<*mut windows::Win32::System::Com::SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }

    fn SetFocus(&self) -> Result<()> {
        let window = HWND(self.hwnd as *mut c_void);
        let _ = unsafe {
            PostMessageW(
                Some(window),
                WM_APP_UIA_FOCUS,
                WPARAM(self.index),
                LPARAM(0),
            )
        };
        Ok(())
    }

    fn FragmentRoot(&self) -> Result<IRawElementProviderFragmentRoot> {
        Ok(RootProvider {
            hwnd: self.hwnd,
            tree: self.tree.clone(),
        }
        .into())
    }
}

#[allow(non_snake_case)]
#[windows::core::implement(IInvokeProvider)]
struct InvokePattern {
    hwnd: isize,
    /// Packed client coordinates of the control center.
    packed: isize,
}

#[allow(non_snake_case)]
impl IInvokeProvider_Impl for InvokePattern_Impl {
    fn Invoke(&self) -> Result<()> {
        let window = HWND(self.hwnd as *mut c_void);
        let _ = unsafe {
            PostMessageW(
                Some(window),
                WM_APP_UIA_ACTIVATE,
                WPARAM(self.packed as usize),
                LPARAM(0),
            )
        };
        Ok(())
    }
}

#[allow(non_snake_case)]
#[windows::core::implement(IRangeValueProvider)]
struct RangePattern {
    hwnd: isize,
    slider: Slider,
    value: f64,
    minimum: f64,
    maximum: f64,
    step: f64,
    page: f64,
    read_only: bool,
}

#[allow(non_snake_case)]
impl IRangeValueProvider_Impl for RangePattern_Impl {
    fn SetValue(&self, val: f64) -> Result<()> {
        if self.read_only {
            return Err(unavailable());
        }
        let value = val.clamp(self.minimum, self.maximum);
        let kind = match self.slider {
            Slider::Volume => 1,
            Slider::Progress => 2,
        };
        let window = HWND(self.hwnd as *mut c_void);
        let _ = unsafe {
            PostMessageW(
                Some(window),
                WM_APP_UIA_RANGE,
                WPARAM(kind),
                LPARAM(value.to_bits() as isize),
            )
        };
        Ok(())
    }

    fn Value(&self) -> Result<f64> {
        Ok(self.value)
    }

    fn IsReadOnly(&self) -> Result<BOOL> {
        Ok(BOOL::from(self.read_only))
    }

    fn Maximum(&self) -> Result<f64> {
        Ok(self.maximum)
    }

    fn Minimum(&self) -> Result<f64> {
        Ok(self.minimum)
    }

    fn LargeChange(&self) -> Result<f64> {
        Ok(self.page)
    }

    fn SmallChange(&self) -> Result<f64> {
        Ok(self.step)
    }
}

/// Announce a keyboard focus change to UIA clients.
pub fn notify_focus(hwnd: HWND, target: Target) {
    let tree = build_tree(hwnd);
    let Some(index) = tree
        .controls
        .iter()
        .position(|control| control.target == Some(target))
    else {
        return;
    };
    let provider: IRawElementProviderSimple = ControlProvider {
        hwnd: hwnd.0 as isize,
        tree,
        index,
    }
    .into();
    let _ = unsafe { UiaRaiseAutomationEvent(&provider, UIA_AutomationFocusChangedEventId) };
}

/// `WM_GETOBJECT` payload for the window's root object id.
pub fn root_object_id() -> i32 {
    UiaRootObjectId
}

pub fn return_root(
    hwnd: HWND,
    wparam: WPARAM,
    lparam: LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    let provider = root(hwnd);
    unsafe { UiaReturnRawElementProvider(hwnd, wparam, lparam, &provider) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_type_ids_match_uia() {
        assert_eq!(UIA_ButtonControlTypeId.0, CONTROL_TYPE_BUTTON);
        assert_eq!(UIA_SliderControlTypeId.0, CONTROL_TYPE_SLIDER);
    }

    #[test]
    fn variant_helpers_set_the_expected_type() {
        let text = variant_bstr("Bamboo");
        assert_eq!(unsafe { text.Anonymous.Anonymous.vt }, VT_BSTR);
        let number = variant_i4(UIA_WindowControlTypeId.0);
        assert_eq!(unsafe { number.Anonymous.Anonymous.vt }, VT_I4);
        let boolean = variant_bool(true);
        assert_eq!(unsafe { boolean.Anonymous.Anonymous.vt }, VT_BOOL);
    }

    #[test]
    fn time_format_is_stable() {
        assert_eq!(format_time(0.0), "0:00");
        assert_eq!(format_time(75.4), "1:15");
    }
}
