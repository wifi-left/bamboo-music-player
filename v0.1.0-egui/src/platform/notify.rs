//! Best-effort Windows toast notifications for track changes.
//!
//! Unpackaged Win32 apps can show toasts through the WinRT notification manager
//! when they have an AppUserModelID (set in `main`). Some systems still refuse
//! without a Start Menu shortcut; failures are logged and the app keeps working.

use windows::Data::Xml::Dom::XmlDocument;
use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
use windows::core::HSTRING;

const APP_ID: &str = "Bamboo.Music.Player";

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Show a two-line toast. Returns immediately; the WinRT calls run on a short
/// worker thread so a slow notification service cannot stall the UI.
pub fn show(title: &str, body: &str) -> bool {
    let xml = format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",
        escape(title),
        escape(body)
    );
    std::thread::Builder::new()
        .name("bamboo-toast".to_string())
        .spawn(move || {
            if let Err(err) = show_blocking(&xml) {
                tracing::warn!("toast notification failed: {err}");
            }
        })
        .is_ok()
}

fn show_blocking(xml: &str) -> windows::core::Result<()> {
    unsafe {
        // S_FALSE means COM was already initialized on this thread; both are
        // fine for a short-lived notification thread.
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
    let document = XmlDocument::new()?;
    document.LoadXml(&HSTRING::from(xml))?;
    let toast = ToastNotification::CreateToastNotification(&document)?;
    let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID))
        .or_else(|_| ToastNotificationManager::CreateToastNotifier())?;
    notifier.Show(&toast)?;
    Ok(())
}
