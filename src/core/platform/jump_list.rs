//! Windows taskbar jump list: quick tasks and recently played files.
//!
//! Jump-list links launch the exe with arguments. The single-instance IPC in
//! `platform::ipc` forwards those arguments to the running window, so clicking
//! "下一首" or a recent file affects the current player instead of starting a
//! second process.

use std::path::Path;

use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::Win32::UI::Shell::Common::{IObjectArray, IObjectCollection};
use windows::Win32::UI::Shell::{
    DestinationList, EnumerableObjectCollection, ICustomDestinationList, IShellLinkW, ShellLink,
};
use windows::core::{HSTRING, IUnknown, Interface};

use crate::config::RecentFile;
use crate::platform::APP_USER_MODEL_ID;

fn jump_tasks() -> [(&'static str, &'static str); 4] {
    [
        ("--open-files", crate::i18n::tr("打开文件…")),
        ("--import-playlist", crate::i18n::tr("导入歌单…")),
        ("--play-pause", crate::i18n::tr("播放 / 暂停")),
        ("--next", crate::i18n::tr("下一首")),
    ]
}

fn com_init() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
}

/// Replace the app's jump list with the current tasks and recent files.
pub fn update(exe: &Path, recent: &[RecentFile]) -> bool {
    update_for(APP_USER_MODEL_ID, exe, recent)
}

fn update_for(app_id: &str, exe: &Path, recent: &[RecentFile]) -> bool {
    com_init();
    match unsafe { update_inner(app_id, exe, recent) } {
        Ok(()) => true,
        Err(err) => {
            tracing::warn!("jump list update failed: {err}");
            false
        }
    }
}

/// Remove the jump list (used when the feature is turned off).
pub fn clear() -> bool {
    delete_for(APP_USER_MODEL_ID)
}

fn delete_for(app_id: &str) -> bool {
    com_init();
    let result = (|| -> windows::core::Result<()> {
        let list: ICustomDestinationList =
            unsafe { CoCreateInstance(&DestinationList, None, CLSCTX_INPROC_SERVER)? };
        unsafe { list.DeleteList(&HSTRING::from(app_id)) }
    })();
    match result {
        Ok(()) => true,
        Err(err) => {
            tracing::debug!("jump list clear failed: {err}");
            false
        }
    }
}

unsafe fn update_inner(
    app_id: &str,
    exe: &Path,
    recent: &[RecentFile],
) -> windows::core::Result<()> {
    let list: ICustomDestinationList =
        unsafe { CoCreateInstance(&DestinationList, None, CLSCTX_INPROC_SERVER)? };
    let _ = unsafe { list.SetAppID(&HSTRING::from(app_id)) };

    let mut min_slots = 0u32;
    let _removed: IObjectArray = unsafe { list.BeginList(&mut min_slots)? };

    let tasks = unsafe { collection()? };
    for (args, description) in jump_tasks() {
        let link = unsafe { create_link(exe, args, description, None)? };
        unsafe { add_link(&tasks, link)? };
    }
    if unsafe { tasks.GetCount().unwrap_or(0) } > 0 {
        let array: IObjectArray = tasks.cast()?;
        let _ = unsafe { list.AddUserTasks(&array) };
    }

    let recent_collection = unsafe { collection()? };
    for item in recent.iter().filter(|item| item.path.is_file()).take(8) {
        let arguments = format!("\"{}\"", item.path.display());
        let title = if item.title.trim().is_empty() {
            item.path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| crate::i18n::tr("未命名").to_string())
        } else {
            item.title.clone()
        };
        let link = unsafe { create_link(exe, &arguments, &title, item.path.parent())? };
        unsafe { add_link(&recent_collection, link)? };
    }
    if unsafe { recent_collection.GetCount().unwrap_or(0) } > 0 {
        let array: IObjectArray = recent_collection.cast()?;
        let _ =
            unsafe { list.AppendCategory(&HSTRING::from(crate::i18n::tr("最近播放")), &array) };
    }

    unsafe { list.CommitList() }
}

unsafe fn collection() -> windows::core::Result<IObjectCollection> {
    unsafe { CoCreateInstance(&EnumerableObjectCollection, None, CLSCTX_INPROC_SERVER) }
}

unsafe fn add_link(collection: &IObjectCollection, link: IShellLinkW) -> windows::core::Result<()> {
    let unknown: IUnknown = link.cast()?;
    unsafe { collection.AddObject(&unknown) }
}

unsafe fn create_link(
    exe: &Path,
    arguments: &str,
    description: &str,
    working_dir: Option<&Path>,
) -> windows::core::Result<IShellLinkW> {
    let link: IShellLinkW = unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)? };
    let exe_text = HSTRING::from(exe.to_string_lossy().as_ref());
    unsafe {
        link.SetPath(&exe_text)?;
        link.SetArguments(&HSTRING::from(arguments))?;
        link.SetDescription(&HSTRING::from(description))?;
        link.SetIconLocation(&exe_text, 0)?;
    }
    if let Some(directory) = working_dir {
        let _ = unsafe {
            link.SetWorkingDirectory(&HSTRING::from(directory.to_string_lossy().as_ref()))
        };
    }
    Ok(link)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commits_and_clears_an_isolated_jump_list() {
        let app_id = format!("Bamboo.Player.Test.{}", std::process::id());
        let exe = std::env::current_exe().expect("current exe");
        let recent = vec![RecentFile {
            path: exe.clone(),
            title: "测试曲目".to_string(),
        }];
        assert!(update_for(&app_id, &exe, &recent), "commit failed");
        assert!(delete_for(&app_id), "delete failed");
    }

    #[test]
    fn builds_shell_links_without_committing_a_list() {
        com_init();
        unsafe {
            let exe = Path::new(r"C:\Windows\notepad.exe");
            let link = create_link(exe, "--test", "测试任务", None).expect("create link");
            let collection = collection().expect("object collection");
            add_link(&collection, link).expect("add link");
            assert_eq!(collection.GetCount().unwrap(), 1);
        }
    }
}
