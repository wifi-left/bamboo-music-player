//! Windows System Media Transport Controls (the media flyout in the
//! notification/taskbar area). Optional and toggleable in settings.

use crossbeam_channel::{Receiver, Sender, unbounded};
use windows::Foundation::{TimeSpan, TypedEventHandler};
use windows::Media::{
    MediaPlaybackStatus, MediaPlaybackType, PlaybackPositionChangeRequestedEventArgs,
    SystemMediaTransportControls, SystemMediaTransportControlsButton,
    SystemMediaTransportControlsButtonPressedEventArgs, SystemMediaTransportControlsDisplayUpdater,
    SystemMediaTransportControlsTimelineProperties,
};
use windows::Storage::Streams::{
    DataWriter, InMemoryRandomAccessStream, RandomAccessStreamReference,
};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::WinRT::{
    ISystemMediaTransportControlsInterop, RO_INIT_SINGLETHREADED, RoInitialize,
};
use windows::core::{HSTRING, factory};

/// One millisecond expressed in the 100 ns units used by WinRT `TimeSpan`.
const TIMESPAN_PER_MS: i64 = 10_000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SmtcEvent {
    Play,
    Pause,
    Next,
    Previous,
    Stop,
    Seek(f64),
}

pub struct SmtcController {
    controls: SystemMediaTransportControls,
    updater: Option<SystemMediaTransportControlsDisplayUpdater>,
    events: Receiver<SmtcEvent>,
    key: String,
    status: Option<MediaPlaybackStatus>,
    thumbnail_key: String,
    thumbnail_rev: u64,
    timeline: Option<(i64, i64)>,
}

impl SmtcController {
    pub fn new(raw: isize) -> Option<Self> {
        unsafe {
            let _ = RoInitialize(RO_INIT_SINGLETHREADED);
        }
        let interop: ISystemMediaTransportControlsInterop =
            factory::<SystemMediaTransportControls, ISystemMediaTransportControlsInterop>().ok()?;
        let controls: SystemMediaTransportControls = unsafe {
            interop
                .GetForWindow(HWND(raw as *mut core::ffi::c_void))
                .ok()?
        };

        let (tx, rx): (Sender<SmtcEvent>, Receiver<SmtcEvent>) = unbounded();
        let button_tx = tx.clone();
        let handler = TypedEventHandler::new(
            move |_sender,
                  args: windows::core::Ref<
                '_,
                SystemMediaTransportControlsButtonPressedEventArgs,
            >| {
                if let Ok(args) = args.ok()
                    && let Ok(button) = args.Button()
                {
                    let event = if button == SystemMediaTransportControlsButton::Play {
                        Some(SmtcEvent::Play)
                    } else if button == SystemMediaTransportControlsButton::Pause {
                        Some(SmtcEvent::Pause)
                    } else if button == SystemMediaTransportControlsButton::Next {
                        Some(SmtcEvent::Next)
                    } else if button == SystemMediaTransportControlsButton::Previous {
                        Some(SmtcEvent::Previous)
                    } else if button == SystemMediaTransportControlsButton::Stop {
                        Some(SmtcEvent::Stop)
                    } else {
                        None
                    };
                    if let Some(event) = event {
                        let _ = button_tx.send(event);
                    }
                }
                Ok(())
            },
        );
        let _ = controls.ButtonPressed(&handler);

        let seek_tx = tx;
        let seek_handler = TypedEventHandler::new(
            move |_sender,
                  args: windows::core::Ref<'_, PlaybackPositionChangeRequestedEventArgs>| {
                if let Ok(args) = args.ok()
                    && let Ok(position) = args.RequestedPlaybackPosition()
                {
                    let secs = position.Duration as f64 / 10_000_000.0;
                    let _ = seek_tx.send(SmtcEvent::Seek(secs.max(0.0)));
                }
                Ok(())
            },
        );
        let _ = controls.PlaybackPositionChangeRequested(&seek_handler);

        let _ = controls.SetIsEnabled(true);
        let _ = controls.SetIsPlayEnabled(true);
        let _ = controls.SetIsPauseEnabled(true);
        let _ = controls.SetIsNextEnabled(true);
        let _ = controls.SetIsPreviousEnabled(true);
        let _ = controls.SetIsStopEnabled(true);

        Some(Self {
            updater: controls.DisplayUpdater().ok(),
            controls,
            events: rx,
            key: String::new(),
            status: None,
            thumbnail_key: String::new(),
            thumbnail_rev: 0,
            timeline: None,
        })
    }

    pub fn poll(&self) -> Option<SmtcEvent> {
        self.events.try_recv().ok()
    }

    pub fn update(
        &mut self,
        title: &str,
        artist: &str,
        album: &str,
        playing: bool,
        has_track: bool,
    ) {
        let key = format!("{title}\u{1f}{artist}\u{1f}{album}\u{1f}{has_track}");
        if key != self.key {
            self.key = key;
            self.timeline = None;
            if let Some(updater) = self.updater.as_ref() {
                let _ = updater.SetType(MediaPlaybackType::Music);
                if let Ok(properties) = updater.MusicProperties() {
                    let _ = properties.SetTitle(&HSTRING::from(title));
                    let _ = properties.SetArtist(&HSTRING::from(artist));
                    let _ = properties.SetAlbumTitle(&HSTRING::from(album));
                }
                let _ = updater.Update();
            }
        }
        let status = match (has_track, playing) {
            (false, _) => MediaPlaybackStatus::Closed,
            (true, true) => MediaPlaybackStatus::Playing,
            (true, false) => MediaPlaybackStatus::Paused,
        };
        if self.status != Some(status) {
            self.status = Some(status);
            let _ = self.controls.SetPlaybackStatus(status);
        }
    }

    /// Publish artwork to the flyout. `png` may be any encoded image; None
    /// falls back to the bundled default cover so a missing cover does not
    /// leave the previous track's artwork on screen.
    pub fn set_thumbnail(&mut self, key: &str, revision: u64, png: Option<&[u8]>) {
        if key == self.thumbnail_key && revision == self.thumbnail_rev {
            return;
        }
        // Mark the key even if the stream/update fails, otherwise a broken
        // shell integration would re-encode the image every UI tick.
        self.thumbnail_key = key.to_string();
        self.thumbnail_rev = revision;
        let bytes = png.unwrap_or(crate::util::image_util::DEFAULT_COVER_PNG);
        let Some(reference) = stream_reference(bytes) else {
            return;
        };
        let Some(updater) = self.updater.as_ref() else {
            return;
        };
        if updater.SetThumbnail(&reference).is_ok() {
            let _ = updater.Update();
        }
    }

    /// Publish position/duration. WinRT interpolates between updates, so a
    /// quarter-second refresh is enough to keep the flyout responsive without
    /// spamming the shell every frame.
    pub fn update_timeline(&mut self, position_secs: f64, duration_secs: Option<f64>) {
        let Some(duration) = duration_secs.filter(|duration| *duration > 0.5) else {
            if self.timeline.take().is_some() {
                let _ = self.write_timeline(0, 0);
            }
            return;
        };
        let position_ms = (position_secs.max(0.0) * 1000.0).round() as i64;
        let duration_ms = (duration * 1000.0).round() as i64;
        if let Some((last_position, last_duration)) = self.timeline
            && last_duration == duration_ms
            && (position_ms - last_position).abs() < 250
        {
            return;
        }
        self.timeline = Some((position_ms, duration_ms));
        let _ = self.write_timeline(position_ms, duration_ms);
    }

    fn write_timeline(&self, position_ms: i64, duration_ms: i64) -> windows::core::Result<()> {
        let timeline = SystemMediaTransportControlsTimelineProperties::new()?;
        let start = TimeSpan { Duration: 0 };
        let end = TimeSpan {
            Duration: duration_ms.saturating_mul(TIMESPAN_PER_MS),
        };
        let position = TimeSpan {
            Duration: position_ms
                .clamp(0, duration_ms.max(0))
                .saturating_mul(TIMESPAN_PER_MS),
        };
        timeline.SetStartTime(start)?;
        timeline.SetEndTime(end)?;
        timeline.SetMinSeekTime(start)?;
        timeline.SetMaxSeekTime(end)?;
        timeline.SetPosition(position)?;
        self.controls.UpdateTimelineProperties(&timeline)
    }
}

/// Encode bytes into an in-memory WinRT stream and wrap it for the media
/// flyout. WinRT requires the stream position to be zero before reading.
fn stream_reference(bytes: &[u8]) -> Option<RandomAccessStreamReference> {
    let stream = InMemoryRandomAccessStream::new().ok()?;
    let output = stream.GetOutputStreamAt(0).ok()?;
    let writer = DataWriter::CreateDataWriter(&output).ok()?;
    writer.WriteBytes(bytes).ok()?;
    writer.StoreAsync().ok()?.join().ok()?;
    writer.FlushAsync().ok()?.join().ok()?;
    let _ = writer.DetachStream();
    stream.Seek(0).ok()?;
    RandomAccessStreamReference::CreateFromStream(&stream).ok()
}

impl Drop for SmtcController {
    fn drop(&mut self) {
        let _ = self.controls.SetIsEnabled(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_cover_into_a_winrt_stream_reference() {
        unsafe {
            let _ = RoInitialize(RO_INIT_SINGLETHREADED);
        }
        let reference = stream_reference(crate::util::image_util::DEFAULT_COVER_PNG);
        assert!(reference.is_some(), "thumbnail stream creation failed");
    }
}
