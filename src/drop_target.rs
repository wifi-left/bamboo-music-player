//! OLE `IDropTarget` for files dragged from Explorer.
//!
//! Registering an OLE drop target gives us the standard drag image (via
//! `IDropTargetHelper`) and live `DragOver` positions, so the queue can show
//! an insertion line instead of only appending on drop. `WM_DROPFILES` stays
//! as a fallback when OLE registration is unavailable.

use std::cell::RefCell;
use std::ffi::c_void;
use std::path::PathBuf;

use windows::Win32::Foundation::{HWND, LPARAM, POINT, POINTL, WPARAM};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CoCreateInstance, DVASPECT_CONTENT, FORMATETC, IDataObject,
    TYMED_HGLOBAL,
};
use windows::Win32::System::Ole::{
    CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE, IDropTarget, IDropTarget_Impl,
    RegisterDragDrop, ReleaseStgMedium,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{
    CLSID_DragDropHelper, DragQueryFileW, HDROP, IDropTargetHelper,
};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
use windows::core::{Ref, Result};

#[windows::core::implement(IDropTarget)]
struct QueueDropTarget {
    hwnd: isize,
    helper: Option<IDropTargetHelper>,
    paths: RefCell<Vec<PathBuf>>,
}

impl QueueDropTarget {
    fn new(hwnd: HWND) -> Self {
        let helper = unsafe {
            CoCreateInstance::<_, IDropTargetHelper>(
                &CLSID_DragDropHelper,
                None,
                CLSCTX_INPROC_SERVER,
            )
        }
        .ok();
        Self {
            hwnd: hwnd.0 as isize,
            helper,
            paths: RefCell::new(Vec::new()),
        }
    }

    fn hwnd(&self) -> HWND {
        HWND(self.hwnd as *mut c_void)
    }
}

impl IDropTarget_Impl for QueueDropTarget_Impl {
    fn DragEnter(
        &self,
        pdataobj: Ref<IDataObject>,
        _grfkeystate: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> Result<()> {
        let hwnd = self.hwnd();
        let paths = pdataobj
            .as_ref()
            .map(read_paths)
            .unwrap_or_default();
        *self.paths.borrow_mut() = paths.clone();
        if paths.is_empty() {
            unsafe { *pdweffect = DROPEFFECT_NONE };
            return Ok(());
        }
        if let Some(state) = unsafe { super::state_mut(hwnd) } {
            let (x, y) = client_dip(hwnd, state.app.dpi, pt);
            state.app.external_drag_begin(hwnd, paths.len());
            state.app.external_drag_move(hwnd, x, y);
        }
        unsafe { *pdweffect = DROPEFFECT_COPY };
        if let Some(helper) = self.helper.as_ref() {
            let point = POINT { x: pt.x, y: pt.y };
            if let Some(data) = pdataobj.as_ref() {
                unsafe {
                    let _ = helper.DragEnter(hwnd, data, &point, DROPEFFECT_COPY);
                }
            }
        }
        Ok(())
    }

    fn DragOver(
        &self,
        _grfkeystate: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> Result<()> {
        let hwnd = self.hwnd();
        if self.paths.borrow().is_empty() {
            unsafe { *pdweffect = DROPEFFECT_NONE };
            return Ok(());
        }
        if let Some(state) = unsafe { super::state_mut(hwnd) } {
            let (x, y) = client_dip(hwnd, state.app.dpi, pt);
            state.app.external_drag_move(hwnd, x, y);
        }
        unsafe { *pdweffect = DROPEFFECT_COPY };
        if let Some(helper) = self.helper.as_ref() {
            let point = POINT { x: pt.x, y: pt.y };
            unsafe {
                let _ = helper.DragOver(&point, DROPEFFECT_COPY);
            }
        }
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        let hwnd = self.hwnd();
        if let Some(state) = unsafe { super::state_mut(hwnd) } {
            state.app.external_drag_leave(hwnd);
        }
        self.paths.borrow_mut().clear();
        if let Some(helper) = self.helper.as_ref() {
            unsafe {
                let _ = helper.DragLeave();
            }
        }
        Ok(())
    }

    fn Drop(
        &self,
        pdataobj: Ref<IDataObject>,
        _grfkeystate: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> Result<()> {
        let hwnd = self.hwnd();
        // Prefer the current data object; fall back to the paths captured at
        // DragEnter for sources that reuse one object.
        let mut paths = pdataobj
            .as_ref()
            .map(read_paths)
            .unwrap_or_default();
        if paths.is_empty() {
            paths = std::mem::take(&mut *self.paths.borrow_mut());
        }
        if let Some(state) = unsafe { super::state_mut(hwnd) } {
            let index = state.app.external_drag_take_index(hwnd);
            if !paths.is_empty() {
                match index {
                    Some(index) => state.app.core.handle(
                        crate::core::app::Command::AddPathsAt { paths, index },
                    ),
                    None => state
                        .app
                        .core
                        .handle(crate::core::app::Command::AddPaths(paths)),
                }
                state.app.refresh_queue_view();
                state.app.invalidate_all(hwnd);
            }
        }
        self.paths.borrow_mut().clear();
        unsafe { *pdweffect = DROPEFFECT_COPY };
        if let Some(helper) = self.helper.as_ref() {
            let point = POINT { x: pt.x, y: pt.y };
            if let Some(data) = pdataobj.as_ref() {
                unsafe {
                    let _ = helper.Drop(data, &point, DROPEFFECT_COPY);
                }
            }
        }
        unsafe {
            let _ = PostMessageW(Some(hwnd), crate::WM_APP_WAKE, WPARAM(0), LPARAM(0));
        }
        Ok(())
    }
}

fn client_dip(hwnd: HWND, dpi: f32, pt: &POINTL) -> (f32, f32) {
    let mut point = POINT { x: pt.x, y: pt.y };
    unsafe {
        let _ = ScreenToClient(hwnd, &mut point);
    }
    let scale = if dpi > 0.0 { dpi / 96.0 } else { 1.0 };
    (point.x as f32 / scale, point.y as f32 / scale)
}

/// Extract CF_HDROP file paths from an OLE data object.
fn read_paths(data: &IDataObject) -> Vec<PathBuf> {
    let format = FORMATETC {
        cfFormat: CF_HDROP.0,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    };
    let Ok(mut medium) = (unsafe { data.GetData(&format) }) else {
        return Vec::new();
    };
    let drop = unsafe { HDROP(medium.u.hGlobal.0 as *mut c_void) };
    let mut paths = Vec::new();
    let count = unsafe { DragQueryFileW(drop, u32::MAX, None) };
    for index in 0..count {
        let length = unsafe { DragQueryFileW(drop, index, None) } as usize;
        if length == 0 {
            continue;
        }
        let mut buffer = vec![0u16; length + 1];
        let written =
            unsafe { DragQueryFileW(drop, index, Some(buffer.as_mut_slice())) } as usize;
        paths.push(PathBuf::from(String::from_utf16_lossy(
            &buffer[..written.min(length)],
        )));
    }
    unsafe { ReleaseStgMedium(&mut medium) };
    paths
}

/// Register the main window as an OLE drop target. The returned interface must
/// be kept alive for as long as the window exists; `None` means OLE is
/// unavailable and the caller keeps `WM_DROPFILES`.
pub fn install(hwnd: HWND) -> Option<IDropTarget> {
    let target: IDropTarget = QueueDropTarget::new(hwnd).into();
    match unsafe { RegisterDragDrop(hwnd, &target) } {
        Ok(()) => Some(target),
        Err(err) => {
            tracing::warn!("OLE drop target unavailable: {err}");
            None
        }
    }
}
