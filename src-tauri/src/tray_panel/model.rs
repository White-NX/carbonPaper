//! Plain data shared by the native painter and application-owned controller.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;
use zeroize::Zeroize;

pub(crate) const WIDTH: f32 = 378.0;
pub(crate) const HEIGHT: f32 = 590.0;

#[derive(Clone, Default)]
pub(crate) struct Labels(pub BTreeMap<&'static str, String>);

impl Labels {
    pub fn get(&self, key: &'static str) -> &str {
        self.0.get(key).map(String::as_str).unwrap_or(key)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReadIdentity {
    pub session: Instant,
    pub database: u64,
}

pub(crate) struct Thumbnail {
    pub width: u32,
    pub height: u32,
    /// Premultiplied BGRA, decoded once outside the window's message loop.
    pub pixels: Vec<u8>,
}

impl Drop for Thumbnail {
    fn drop(&mut self) {
        self.pixels.zeroize();
    }
}

pub(crate) struct Record {
    pub id: i64,
    pub timestamp_ms: i64,
    pub time: String,
    pub date: String,
    pub title: String,
    pub app: String,
    pub process: String,
    pub thumbnail: Option<Arc<Thumbnail>>,
}

impl Drop for Record {
    fn drop(&mut self) {
        self.title.zeroize();
        self.app.zeroize();
        self.process.zeroize();
        self.time.zeroize();
        self.date.zeroize();
    }
}

#[derive(Clone, Default)]
pub(crate) struct PanelModel {
    pub labels: Labels,
    pub dark: bool,
    pub lightweight: bool,
    pub running: bool,
    pub paused: bool,
    pub power_saving: bool,
    pub controls_enabled: bool,
    pub ready: bool,
    pub records_loaded: bool,
    pub status: &'static str,
    pub busy: bool,
    pub authenticating: bool,
    pub identity: Option<ReadIdentity>,
    pub records: Vec<Arc<Record>>,
    pub selected: usize,
    pub pause_until_ms: Option<i64>,
    pub excluded: bool,
    pub message: Option<String>,
}

impl PanelModel {
    pub fn capture_controls_enabled(&self) -> bool {
        self.controls_enabled && !self.power_saving
    }

    pub fn capture_button_label(&self) -> &str {
        self.labels.get(if self.power_saving {
            "powerSaving"
        } else if !self.running {
            "start"
        } else if self.paused {
            "resume"
        } else {
            "pause15"
        })
    }

    pub fn selected(&self) -> Option<&Arc<Record>> {
        self.records.get(self.selected)
    }

    pub fn clear_records(&mut self) {
        self.records.clear();
        self.identity = None;
        self.selected = 0;
        self.excluded = false;
        self.records_loaded = false;
    }

    pub fn status_label(&self) -> String {
        if self.status != "paused" {
            return self.labels.get(self.status).to_string();
        }
        match self
            .pause_until_ms
            .and_then(chrono::DateTime::from_timestamp_millis)
        {
            Some(until) => self.labels.get("pausedUntil").replace(
                "{{time}}",
                &until
                    .with_timezone(&chrono::Local)
                    .format("%H:%M")
                    .to_string(),
            ),
            None => self.labels.get("pausedManual").to_string(),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Visibility(bool),
    Select(usize),
    Preview,
    Search(String),
    OpenTimeline,
    ToggleLightweight,
    Pause(u32),
    Start,
    Resume,
    Unlock,
    Lock,
    Settings,
    Restart,
    ToggleExcluded,
    Quit,
    Refresh,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
}

pub(crate) const LIGHTWEIGHT: u16 = 1;
pub(crate) const MORE: u16 = 2;
pub(crate) const SEARCH: u16 = 3;
pub(crate) const SEARCH_SUBMIT: u16 = 4;
pub(crate) const PREVIEW: u16 = 5;
pub(crate) const RECORD_FIRST: u16 = 6;
pub(crate) const PAUSE: u16 = 9;
pub(crate) const PAUSE_MORE: u16 = 10;
pub(crate) const TIMELINE: u16 = 11;
pub(crate) const EXCLUDE: u16 = 12;
pub(crate) const MENU_LOCK: u16 = 101;
pub(crate) const MENU_SETTINGS: u16 = 102;
pub(crate) const MENU_RESTART: u16 = 103;
pub(crate) const MENU_QUIT: u16 = 104;
pub(crate) const MENU_PAUSE_MANUAL: u16 = 100;
pub(crate) const MENU_PAUSE_15: u16 = 115;
pub(crate) const MENU_PAUSE_30: u16 = 130;
pub(crate) const MENU_PAUSE_60: u16 = 160;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Menu {
    More,
    Pause,
}

impl Menu {
    pub fn rect(self) -> Rect {
        match self {
            Self::More => Rect::new(152.0, 44.0, 216.0, 205.0),
            Self::Pause => Rect::new(160.0, 326.0, 202.0, 158.0),
        }
    }

    pub fn controls(self) -> &'static [u16] {
        match self {
            Self::More => &[
                LIGHTWEIGHT,
                MENU_LOCK,
                MENU_SETTINGS,
                MENU_RESTART,
                MENU_QUIT,
            ],
            Self::Pause => &[
                MENU_PAUSE_15,
                MENU_PAUSE_30,
                MENU_PAUSE_60,
                MENU_PAUSE_MANUAL,
            ],
        }
    }

    pub fn trigger(self) -> u16 {
        match self {
            Self::More => MORE,
            Self::Pause => PAUSE_MORE,
        }
    }
}

pub(crate) fn is_menu_control(id: u16) -> bool {
    Menu::More.controls().contains(&id) || Menu::Pause.controls().contains(&id)
}

pub(crate) fn control_rect(id: u16) -> Rect {
    match id {
        MORE => Rect::new(332.0, 8.0, 30.0, 30.0),
        SEARCH => Rect::new(45.0, 70.0, 273.0, 23.0),
        SEARCH_SUBMIT => Rect::new(327.0, 66.0, 30.0, 30.0),
        PREVIEW => Rect::new(16.0, 144.0, 346.0, 222.0),
        RECORD_FIRST..=8 => Rect::new(
            16.0 + f32::from(id - RECORD_FIRST) * 117.0,
            376.0,
            112.0,
            50.0,
        ),
        PAUSE => Rect::new(16.0, 490.0, 304.0, 46.0),
        PAUSE_MORE => Rect::new(320.0, 490.0, 42.0, 46.0),
        TIMELINE => Rect::new(16.0, 548.0, 118.0, 30.0),
        EXCLUDE => Rect::new(194.0, 548.0, 168.0, 30.0),
        LIGHTWEIGHT => Rect::new(157.0, 49.0, 206.0, 34.0),
        MENU_LOCK => Rect::new(157.0, 95.0, 206.0, 34.0),
        MENU_SETTINGS => Rect::new(157.0, 129.0, 206.0, 34.0),
        MENU_RESTART => Rect::new(157.0, 163.0, 206.0, 34.0),
        MENU_QUIT => Rect::new(157.0, 209.0, 206.0, 34.0),
        MENU_PAUSE_15 => Rect::new(165.0, 331.0, 192.0, 34.0),
        MENU_PAUSE_30 => Rect::new(165.0, 365.0, 192.0, 34.0),
        MENU_PAUSE_60 => Rect::new(165.0, 399.0, 192.0, 34.0),
        MENU_PAUSE_MANUAL => Rect::new(165.0, 445.0, 192.0, 34.0),
        _ => Rect::default(),
    }
}

/// Position in physical pixels, including negative-coordinate monitors.
pub(crate) fn popup_position(
    anchor: (i32, i32),
    work: (i32, i32, i32, i32),
    size: (i32, i32),
) -> (i32, i32) {
    let (left, top, right, bottom) = work;
    let x = (anchor.0 - size.0).clamp(left + 8, (right - size.0 - 8).max(left + 8));
    let y = (anchor.1 - size.1 - 8).clamp(top + 8, (bottom - size.1 - 8).max(top + 8));
    (x, y)
}

pub(crate) fn fit_scale(dpi_scale: f32, work: (i32, i32, i32, i32)) -> f32 {
    dpi_scale
        .min((work.2 - work.0 - 16).max(1) as f32 / WIDTH)
        .min((work.3 - work.1 - 16).max(1) as f32 / HEIGHT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popups_stay_in_the_work_area_on_negative_and_high_dpi_monitors() {
        let work = (-2560, -200, 0, 1240);
        for anchor in [(-10, 1240), (-2550, -180), (-1200, 500)] {
            let (x, y) = popup_position(anchor, work, (561, 873));
            assert!(x >= work.0 && y >= work.1);
            assert!(x + 561 <= work.2 && y + 873 <= work.3);
        }
        let small = (0, 0, 1024, 600);
        let scale = fit_scale(2.0, small);
        let size = (
            (WIDTH * scale).round() as i32,
            (HEIGHT * scale).round() as i32,
        );
        let (x, y) = popup_position((1024, 600), small, size);
        assert!(x >= 0 && y >= 0 && x + size.0 <= 1024 && y + size.1 <= 600);
    }
}
