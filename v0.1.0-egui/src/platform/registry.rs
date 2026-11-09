//! Per-user Windows integration: run-on-login and audio file associations.
//!
//! Everything is written under `HKEY_CURRENT_USER`, so no elevation is needed
//! and uninstalling/removing the values is always possible. Disabling file
//! associations removes only our ProgID keys and the per-extension default
//! value; existing system associations fall back as usual.

use anyhow::{Result, anyhow};
use windows::Win32::Foundation::WIN32_ERROR;
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegDeleteTreeW, RegGetValueW,
    RegSetKeyValueW,
};
use windows::core::HSTRING;

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_VALUE: &str = "BambooMusicPlayer";
const CLASSES: &str = "Software\\Classes";
const PROG_PREFIX: &str = "BambooMusicPlayer";
const PLAYLIST_EXTENSION: &str = "mp3list";
const PLAYLIST_PROG_ID: &str = "BambooMusicPlayer.mp3list";
const EXTENSIONS: [&str; 9] = [
    "mp3", "flac", "wav", "ogg", "m4a", "aac", "opus", "wma", "aiff",
];

fn ok(status: WIN32_ERROR) -> Result<()> {
    match status.0 {
        0 => Ok(()),
        // ERROR_ACCESS_DENIED: the per-user Classes key can be locked down by
        // group policy. Tell the user instead of showing a raw Win32 code.
        5 => Err(anyhow!(
            "拒绝访问：需要管理员权限，或注册表被组策略锁定"
        )),
        code => Err(anyhow!("注册表操作失败 (Win32 {code})")),
    }
}

fn set_string(key: &HSTRING, name: &HSTRING, data: &str) -> Result<()> {
    let wide: Vec<u16> = data.encode_utf16().chain(std::iter::once(0)).collect();
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            key,
            name,
            REG_SZ.0,
            Some(wide.as_ptr() as *const core::ffi::c_void),
            (wide.len() * std::mem::size_of::<u16>()) as u32,
        )
    };
    ok(status)
}

fn delete_value(key: &HSTRING, name: &HSTRING) -> Result<()> {
    let status = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key, name) };
    // Missing values are fine.
    if status.0 == 0 || status.0 == 2 {
        Ok(())
    } else {
        Err(anyhow!("删除注册表值失败 (Win32 {})", status.0))
    }
}

fn delete_tree(key: &HSTRING) -> Result<()> {
    let status = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, key) };
    if status.0 == 0 || status.0 == 2 {
        Ok(())
    } else {
        Err(anyhow!("删除注册表项失败 (Win32 {})", status.0))
    }
}

fn get_string(key: &HSTRING, name: &HSTRING) -> Option<String> {
    let mut size = 0u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key,
            name,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        )
    };
    if status.0 != 0 || size == 0 {
        return None;
    }
    let mut buffer = vec![0u16; (size as usize / 2).max(1)];
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key,
            name,
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr() as *mut core::ffi::c_void),
            Some(&mut size),
        )
    };
    if status.0 != 0 {
        return None;
    }
    let end = buffer
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(buffer.len());
    String::from_utf16(&buffer[..end]).ok()
}

fn exe_path() -> String {
    std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "bamboo-player.exe".to_string())
}

pub fn autostart_enabled() -> bool {
    let key = HSTRING::from(RUN_KEY);
    let name = HSTRING::from(RUN_VALUE);
    get_string(&key, &name).is_some()
}

pub fn set_autostart(enabled: bool) -> Result<()> {
    let key = HSTRING::from(RUN_KEY);
    let name = HSTRING::from(RUN_VALUE);
    if enabled {
        set_string(&key, &name, &format!("\"{}\"", exe_path()))
    } else {
        delete_value(&key, &name)
    }
}

pub fn associations_enabled() -> bool {
    let command = format!("{CLASSES}\\{PROG_PREFIX}.mp3\\shell\\open\\command");
    let key = HSTRING::from(command);
    let name = HSTRING::from("");
    get_string(&key, &name).is_some()
}

pub fn set_associations(enabled: bool) -> Result<()> {
    let exe = exe_path();
    for extension in EXTENSIONS {
        let prog_id = format!("{PROG_PREFIX}.{extension}");
        let ext_key = HSTRING::from(format!("{CLASSES}\\.{extension}"));
        let prog_key = HSTRING::from(format!("{CLASSES}\\{prog_id}"));
        let command_key = HSTRING::from(format!("{CLASSES}\\{prog_id}\\shell\\open\\command"));
        let icon_key = HSTRING::from(format!("{CLASSES}\\{prog_id}\\DefaultIcon"));
        let empty = HSTRING::from("");
        if enabled {
            set_string(&ext_key, &empty, &prog_id)?;
            set_string(
                &prog_key,
                &empty,
                &format!("Bamboo Music Player ({extension})"),
            )?;
            set_string(&icon_key, &empty, &format!("\"{exe}\",0"))?;
            set_string(&command_key, &empty, &format!("\"{exe}\" \"%1\""))?;
        } else {
            delete_value(&ext_key, &empty)?;
            delete_tree(&prog_key)?;
        }
    }
    Ok(())
}

/// Whether `.mp3list` playlists are registered to open in this player.
pub fn playlist_association_enabled() -> bool {
    let command = format!("{CLASSES}\\{PLAYLIST_PROG_ID}\\shell\\open\\command");
    let key = HSTRING::from(command);
    let name = HSTRING::from("");
    get_string(&key, &name).is_some()
}

/// Register/unregister `.mp3list` separately from audio files. Playlists are
/// not media, so this uses its own ProgID and can be toggled on its own.
///
/// Everything stays under `HKEY_CURRENT_USER\Software\Classes`, which normally
/// needs no elevation; if group policy denies it, the error tells the user to
/// run as administrator.
pub fn set_playlist_association(enabled: bool) -> Result<()> {
    let exe = exe_path();
    let ext_key = HSTRING::from(format!("{CLASSES}\\.{PLAYLIST_EXTENSION}"));
    let prog_key = HSTRING::from(format!("{CLASSES}\\{PLAYLIST_PROG_ID}"));
    let command_key = HSTRING::from(format!("{CLASSES}\\{PLAYLIST_PROG_ID}\\shell\\open\\command"));
    let icon_key = HSTRING::from(format!("{CLASSES}\\{PLAYLIST_PROG_ID}\\DefaultIcon"));
    let empty = HSTRING::from("");
    if enabled {
        set_string(&ext_key, &empty, PLAYLIST_PROG_ID)?;
        set_string(
            &prog_key,
            &empty,
            "Bamboo Music Player 歌单 (.mp3list)",
        )?;
        set_string(&icon_key, &empty, &format!("\"{exe}\",0"))?;
        set_string(&command_key, &empty, &format!("\"{exe}\" \"%1\""))?;
    } else {
        delete_value(&ext_key, &empty)?;
        delete_tree(&prog_key)?;
    }
    Ok(())
}
