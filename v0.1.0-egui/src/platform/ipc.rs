//! Forward command-line arguments from a second instance to the running one.
//!
//! The single-instance guard makes a second launch exit immediately. Before it
//! does, it writes its arguments to a named pipe owned by the first instance.
//! That makes jump-list items, file associations and "open with" work while the
//! player is already running. A named pipe is used instead of a window message
//! because the window title changes with the current track.

use std::ffi::OsString;
use std::os::windows::ffi::OsStrExt;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, unbounded};
use windows::Win32::Foundation::{CloseHandle, ERROR_PIPE_CONNECTED, GENERIC_WRITE, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_MODE, OPEN_EXISTING, PIPE_ACCESS_INBOUND,
    ReadFile, WriteFile,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, NAMED_PIPE_MODE, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows::core::PCWSTR;

const MAX_MESSAGE_BYTES: usize = 256 * 1024;

/// A closed receiver for multi-instance mode, where there is no primary
/// window to forward commands to.
pub fn disabled() -> Receiver<Vec<String>> {
    let (_sender, receiver) = unbounded();
    receiver
}

/// Start the first-instance pipe server and return the command receiver.
pub fn start_server() -> Receiver<Vec<String>> {
    let (tx, rx) = unbounded();
    let name = pipe_name();
    std::thread::Builder::new()
        .name("bamboo-ipc".to_string())
        .spawn(move || serve(name, tx))
        .ok();
    rx
}

/// Write `args` to the running instance's pipe. Retries briefly because the
/// first instance may still be creating the pipe while the second launches.
pub fn send_to_existing(args: &[OsString]) -> bool {
    if args.is_empty() {
        return false;
    }
    let payload = encode_args(args);
    if payload.is_empty() {
        return false;
    }
    let mut bytes = Vec::with_capacity(payload.len() * 2);
    for unit in payload {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    let name = pipe_name();
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    for _ in 0..60 {
        let handle = unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                GENERIC_WRITE.0,
                FILE_SHARE_MODE(0),
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            )
        };
        if let Ok(handle) = handle {
            let mut written = 0u32;
            let ok = unsafe {
                WriteFile(handle, Some(&bytes), Some(&mut written), None).is_ok()
                    && written as usize == bytes.len()
            };
            let _ = unsafe { CloseHandle(handle) };
            if ok {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn pipe_name() -> String {
    format!(
        "\\\\.\\pipe\\{}",
        crate::platform::single_instance::instance_name()
    )
}

fn serve(name: String, sender: Sender<Vec<String>>) {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let mode = NAMED_PIPE_MODE(
        PIPE_TYPE_BYTE.0 | PIPE_READMODE_BYTE.0 | PIPE_WAIT.0 | PIPE_REJECT_REMOTE_CLIENTS.0,
    );
    let pipe: HANDLE = unsafe {
        CreateNamedPipeW(
            PCWSTR(wide.as_ptr()),
            PIPE_ACCESS_INBOUND,
            mode,
            1,
            MAX_MESSAGE_BYTES as u32,
            MAX_MESSAGE_BYTES as u32,
            0,
            None,
        )
    };
    if pipe.is_invalid() {
        tracing::warn!("could not create IPC pipe {name}");
        return;
    }
    loop {
        let connected = match unsafe { ConnectNamedPipe(pipe, None) } {
            Ok(()) => true,
            Err(err) => (err.code().0 as u32) & 0xFFFF == ERROR_PIPE_CONNECTED.0,
        };
        if connected {
            let mut buffer = vec![0u8; MAX_MESSAGE_BYTES];
            let mut total = 0usize;
            loop {
                let mut read = 0u32;
                let ok =
                    unsafe { ReadFile(pipe, Some(&mut buffer[total..]), Some(&mut read), None) };
                if ok.is_err() || read == 0 {
                    break;
                }
                total += read as usize;
                if total >= buffer.len() {
                    break;
                }
            }
            if total >= 2 {
                let units: Vec<u16> = buffer[..total - (total % 2)]
                    .chunks_exact(2)
                    .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                    .collect();
                let text = String::from_utf16_lossy(&units);
                let args = decode_args(&text);
                if !args.is_empty() {
                    let _ = sender.send(args);
                }
            }
            unsafe {
                let _ = DisconnectNamedPipe(pipe);
            }
        } else {
            // ERROR_PIPE_CONNECTED is reported when the client connected
            // before ConnectNamedPipe; the next read still works. Other errors
            // (broken handle) are retried after a short pause.
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Arguments are UTF-16 and separated by `\n`; Windows paths cannot contain a
/// newline, so this stays lossless for file names.
pub fn encode_args(args: &[OsString]) -> Vec<u16> {
    let mut out = Vec::new();
    for (index, arg) in args.iter().enumerate() {
        if index > 0 {
            out.push('\n' as u16);
        }
        out.extend(arg.as_os_str().encode_wide());
    }
    out
}

pub fn decode_args(text: &str) -> Vec<String> {
    text.split('\n')
        .filter(|arg| !arg.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argument_payload_round_trips() {
        let args = vec![
            OsString::from("C:\\Music\\Artist - Song.mp3"),
            OsString::from("--play-pause"),
            OsString::from("F:\\新建文件夹\\测试.flac"),
        ];
        let encoded = encode_args(&args);
        let text = String::from_utf16(&encoded).unwrap();
        assert_eq!(
            decode_args(&text),
            vec![
                "C:\\Music\\Artist - Song.mp3".to_string(),
                "--play-pause".to_string(),
                "F:\\新建文件夹\\测试.flac".to_string(),
            ]
        );
    }

    #[test]
    fn pipe_round_trip_between_instances() {
        unsafe {
            std::env::set_var(
                "BAMBOO_INSTANCE",
                format!("bamboo-ipc-test-{}", std::process::id()),
            );
        }
        let receiver = start_server();
        std::thread::sleep(Duration::from_millis(100));
        let args = vec![
            OsString::from("--show"),
            OsString::from(r"C:\Music\IPC Test.mp3"),
        ];
        assert!(send_to_existing(&args), "pipe write failed");
        let received = receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("forwarded arguments");
        assert_eq!(
            received,
            vec!["--show".to_string(), r"C:\Music\IPC Test.mp3".to_string(),]
        );
        unsafe {
            std::env::remove_var("BAMBOO_INSTANCE");
        }
    }

    #[test]
    fn empty_arguments_are_ignored() {
        assert!(encode_args(&[]).is_empty());
        assert!(decode_args("").is_empty());
        assert_eq!(decode_args("\n\n"), Vec::<String>::new());
    }
}
