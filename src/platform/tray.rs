//! System tray icon and menu.

use std::time::Duration;

use crossbeam_channel::{unbounded, Receiver};
use muda::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{
    MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent, Icon,
};

use crate::config::Language;
use crate::i18n::t;
use crate::model::RepeatMode;

#[derive(Clone, Debug)]
pub enum TrayAction {
    ShowWindow,
    HideWindow,
    ToggleWindow,
    PlayPause,
    Prev,
    Next,
    ToggleShuffle,
    CycleRepeat,
    ToggleDesktopLyrics,
    ToggleLockLyrics,
    Volume(f32),
    Quit,
}

pub struct Tray {
    _icon: TrayIcon,
    play_item: MenuItem,
    repeat_item: MenuItem,
    shuffle_item: CheckMenuItem,
    desktop_item: CheckMenuItem,
    lock_item: CheckMenuItem,
    actions: Receiver<TrayAction>,
    lang: Language,
}

impl Tray {
    pub fn new(ctx: egui::Context, lang: Language, accent: [u8; 3]) -> anyhow::Result<Self> {
        let menu = Menu::new();

        let show = MenuItem::with_id("show", t(lang, "tray.show"), true, None);
        let hide = MenuItem::with_id("hide", t(lang, "tray.hide"), true, None);
        let play = MenuItem::with_id("play", t(lang, "tray.play_pause"), true, None);
        let prev = MenuItem::with_id("prev", t(lang, "tray.prev"), true, None);
        let next = MenuItem::with_id("next", t(lang, "tray.next"), true, None);
        let desktop = CheckMenuItem::with_id(
            "desktop",
            t(lang, "tray.desktop_lyrics"),
            true,
            false,
            None,
        );
        let lock = CheckMenuItem::with_id(
            "lock",
            t(lang, "tray.lock_lyrics"),
            true,
            false,
            None,
        );
        let shuffle = CheckMenuItem::with_id("shuffle", t(lang, "tray.shuffle"), true, false, None);
        let repeat = MenuItem::with_id("repeat", t(lang, "tray.repeat"), true, None);
        let volume = Submenu::with_id("volume", t(lang, "tray.volume"), true);
        for percent in [0, 25, 50, 75, 100] {
            let item = MenuItem::with_id(
                format!("vol_{percent}"),
                format!("{percent}%"),
                true,
                None,
            );
            volume.append(&item)?;
        }
        let quit = MenuItem::with_id("quit", t(lang, "tray.exit"), true, None);

        menu.append_items(&[
            &show,
            &hide,
            &PredefinedMenuItem::separator(),
            &play,
            &prev,
            &next,
            &PredefinedMenuItem::separator(),
            &desktop,
            &lock,
            &shuffle,
            &repeat,
            &volume,
            &PredefinedMenuItem::separator(),
            &quit,
        ])?;

        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Bamboo Music Player")
            .with_icon(make_icon(accent)?)
            .with_menu_on_left_click(false)
            .build()?;

        let (tx, rx) = unbounded();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("bamboo-tray".to_string())
            .spawn(move || {
                loop {
                    let mut fired = false;
                    while let Ok(event) = MenuEvent::receiver().try_recv() {
                        if let Some(action) = map_menu(&event.id.0) {
                            let _ = tx.send(action);
                            fired = true;
                        }
                    }
                    while let Ok(event) = TrayIconEvent::receiver().try_recv() {
                        match event {
                            TrayIconEvent::Click {
                                button: MouseButton::Left,
                                button_state: MouseButtonState::Up,
                                ..
                            } => {
                                let _ = tx.send(TrayAction::ToggleWindow);
                                fired = true;
                            }
                            TrayIconEvent::DoubleClick { .. } => {
                                let _ = tx.send(TrayAction::PlayPause);
                                fired = true;
                            }
                            _ => {}
                        }
                    }
                    if fired {
                        ctx.request_repaint();
                    }
                    std::thread::sleep(Duration::from_millis(80));
                }
            })
            .ok();

        Ok(Self {
            _icon: icon,
            play_item: play,
            repeat_item: repeat,
            shuffle_item: shuffle,
            desktop_item: desktop,
            lock_item: lock,
            actions: rx,
            lang,
        })
    }

    pub fn poll(&self) -> Vec<TrayAction> {
        let mut out = Vec::new();
        while let Ok(action) = self.actions.try_recv() {
            out.push(action);
        }
        out
    }

    pub fn set_playing(&self, playing: bool) {
        self.play_item.set_text(if playing {
            format!("{} / {}", t(self.lang, "player.pause"), t(self.lang, "tray.play_pause"))
        } else {
            format!("{} / {}", t(self.lang, "player.play"), t(self.lang, "tray.play_pause"))
        });
    }

    pub fn set_tooltip(&self, text: &str) {
        let _ = self._icon.set_tooltip(Some(text));
    }

    pub fn set_shuffle(&self, on: bool) {
        self.shuffle_item.set_checked(on);
    }

    pub fn set_repeat(&self, mode: RepeatMode) {
        let label = match mode {
            RepeatMode::Off => t(self.lang, "common.off"),
            RepeatMode::All => t(self.lang, "player.repeat"),
            RepeatMode::One => "1",
        };
        self.repeat_item.set_text(format!(
            "{}: {}",
            t(self.lang, "tray.repeat"),
            label
        ));
    }

    pub fn set_desktop_lyrics(&self, on: bool) {
        self.desktop_item.set_checked(on);
    }

    pub fn set_locked(&self, on: bool) {
        self.lock_item.set_checked(on);
    }
}

fn map_menu(id: &str) -> Option<TrayAction> {
    match id {
        "show" => Some(TrayAction::ShowWindow),
        "hide" => Some(TrayAction::HideWindow),
        "play" => Some(TrayAction::PlayPause),
        "prev" => Some(TrayAction::Prev),
        "next" => Some(TrayAction::Next),
        "desktop" => Some(TrayAction::ToggleDesktopLyrics),
        "lock" => Some(TrayAction::ToggleLockLyrics),
        "shuffle" => Some(TrayAction::ToggleShuffle),
        "repeat" => Some(TrayAction::CycleRepeat),
        "quit" => Some(TrayAction::Quit),
        other => other
            .strip_prefix("vol_")
            .and_then(|v| v.parse::<f32>().ok())
            .map(|p| TrayAction::Volume(p / 100.0)),
    }
}

fn make_icon(accent: [u8; 3]) -> anyhow::Result<Icon> {
    const SIZE: usize = 32;
    let mut rgba = vec![0u8; SIZE * SIZE * 4];
    let cx = (SIZE as f32 - 1.0) / 2.0;
    let cy = (SIZE as f32 - 1.0) / 2.0;
    let radius = 14.5f32;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist <= radius {
                // Slight top-down gradient for depth.
                let t = (y as f32 / SIZE as f32).clamp(0.0, 1.0);
                let f = 1.12 - t * 0.28;
                let idx = (y * SIZE + x) * 4;
                rgba[idx] = (accent[0] as f32 * f).clamp(0.0, 255.0) as u8;
                rgba[idx + 1] = (accent[1] as f32 * f).clamp(0.0, 255.0) as u8;
                rgba[idx + 2] = (accent[2] as f32 * f).clamp(0.0, 255.0) as u8;
                rgba[idx + 3] = 255;
            }
        }
    }
    // White play triangle.
    let tri = [(11.5f32, 8.5f32), (11.5, 23.5), (24.0, 16.0)];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let p = (x as f32 + 0.5, y as f32 + 0.5);
            if point_in_triangle(p, tri[0], tri[1], tri[2]) {
                let idx = (y * SIZE + x) * 4;
                rgba[idx] = 255;
                rgba[idx + 1] = 255;
                rgba[idx + 2] = 255;
                rgba[idx + 3] = 255;
            }
        }
    }
    Ok(Icon::from_rgba(rgba, SIZE as u32, SIZE as u32)?)
}

fn point_in_triangle(p: (f32, f32), a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> bool {
    fn sign(p1: (f32, f32), p2: (f32, f32), p3: (f32, f32)) -> f32 {
        (p1.0 - p3.0) * (p2.1 - p3.1) - (p2.0 - p3.0) * (p1.1 - p3.1)
    }
    let d1 = sign(p, a, b);
    let d2 = sign(p, b, c);
    let d3 = sign(p, c, a);
    let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_neg && has_pos)
}
