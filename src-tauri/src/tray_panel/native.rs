//! HWND ownership, standard accessible controls, IME, focus and popup placement.
//! Other threads only post messages; all drawing and HWND mutations run here.

use super::model::*;
use super::render::{ControlVisual, Palette, Renderer};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::*;
use zeroize::Zeroizing;

const UPDATE: u32 = WM_APP + 41;
const SHOW: u32 = WM_APP + 42;
const HIDE: u32 = WM_APP + 43;
const FOCUS: u32 = WM_APP + 44;
const DESTROY: u32 = WM_APP + 45;
const RESTORE_OWNER: u32 = WM_APP + 46;
const VISIBLE_TIMER: usize = 1;
const IDS: [u16; 20] = [
    MORE,
    SEARCH,
    SEARCH_SUBMIT,
    PREVIEW,
    6,
    7,
    8,
    PAUSE,
    PAUSE_MORE,
    TIMELINE,
    EXCLUDE,
    LIGHTWEIGHT,
    MENU_LOCK,
    MENU_SETTINGS,
    MENU_RESTART,
    MENU_QUIT,
    MENU_PAUSE_15,
    MENU_PAUSE_30,
    MENU_PAUSE_60,
    MENU_PAUSE_MANUAL,
];

struct Context {
    model: Arc<Mutex<PanelModel>>,
    action: Box<dyn Fn(Action)>,
    authorized: Box<dyn Fn(ReadIdentity) -> bool>,
    attached: Rc<Cell<bool>>,
    renderer: RefCell<Option<Renderer>>,
    controls: RefCell<BTreeMap<u16, HWND>>,
    scale: Cell<f32>,
    font: Cell<HFONT>,
    edit_brush: Cell<HBRUSH>,
    background_brush: Cell<HBRUSH>,
    dark: Cell<Option<bool>>,
    hover: Cell<Option<u16>>,
    visible: Cell<bool>,
    menu: Cell<Option<Menu>>,
    composing: Cell<bool>,
    cue_supported: Cell<bool>,
    cue: RefCell<String>,
    was_busy: Cell<bool>,
    pending_focus: Cell<Option<u16>>,
    keyboard_focus: Cell<bool>,
    show_started: Cell<Option<Instant>>,
}

impl Context {
    unsafe fn control_states(&self) -> Vec<ControlVisual> {
        let focus = GetFocus();
        self.controls
            .borrow()
            .iter()
            .filter(|(id, child)| **id != SEARCH && visible_style(**child))
            .map(|(&id, &child)| ControlVisual {
                id,
                focused: focus == child && self.keyboard_focus.get(),
                hovered: self.hover.get() == Some(id),
                pressed: SendMessageW(child, BM_GETSTATE, WPARAM(0), LPARAM(0)).0
                    & BST_PUSHED as isize
                    != 0,
                enabled: IsWindowEnabled(child).as_bool(),
            })
            .collect()
    }

    fn snapshot(&self) -> PanelModel {
        let mut model = self.model.lock().unwrap_or_else(|e| e.into_inner());
        if model
            .identity
            .is_some_and(|identity| !(self.authorized)(identity))
        {
            model.clear_records();
        }
        model.clone()
    }

    unsafe fn invalidate(&self, hwnd: HWND) {
        let _ = InvalidateRect(hwnd, None, false);
        for (&id, &child) in self.controls.borrow().iter() {
            if id != SEARCH && visible_style(child) {
                let _ = InvalidateRect(child, None, false);
            }
        }
    }

    unsafe fn synchronize(&self, hwnd: HWND) {
        let model = self.snapshot();
        // Let the credential dialog own the foreground while it is open.
        let topmost = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0 != 0;
        let desired_topmost = !model.authenticating;
        if topmost != desired_topmost {
            let _ = SetWindowPos(
                hwnd,
                if desired_topmost {
                    HWND_TOPMOST
                } else {
                    HWND_NOTOPMOST
                },
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
        let was_busy = self.was_busy.replace(model.busy);
        if model.busy && !was_busy {
            let focus = GetFocus();
            self.pending_focus.set(
                self.controls
                    .borrow()
                    .iter()
                    .find_map(|(&id, &child)| (child == focus).then_some(id)),
            );
        }
        if let Some(renderer) = self.renderer.borrow_mut().as_mut() {
            renderer.synchronize(&model);
        }
        if self.dark.get() != Some(model.dark) {
            self.dark.set(Some(model.dark));
            let palette = Palette::new(model.dark);
            for (slot, color) in [
                (&self.edit_brush, palette.paper),
                (&self.background_brush, palette.background),
            ] {
                let old = slot.replace(CreateSolidBrush(color_ref(color)));
                if !old.is_invalid() {
                    let _ = DeleteObject(old);
                }
            }
            apply_frame(hwnd, model.dark);
            if let Some(edit) = self.controls.borrow().get(&SEARCH) {
                let _ = InvalidateRect(*edit, None, true);
            }
        }
        if model.busy && self.menu.take().is_some() {
            self.hover.set(None);
        }
        for (&id, &child) in self.controls.borrow().iter() {
            let visible = if is_menu_control(id) {
                self.menu
                    .get()
                    .is_some_and(|menu| menu.controls().contains(&id))
            } else if id == SEARCH || id == SEARCH_SUBMIT {
                self.menu.get() != Some(Menu::More)
            } else if (RECORD_FIRST..=8).contains(&id) {
                model.identity.is_some() && usize::from(id - RECORD_FIRST) < model.records.len()
            } else {
                true
            };
            if visible_style(child) != visible {
                let _ = ShowWindow(child, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
            }
            let enabled = !model.busy
                && match id {
                    SEARCH | SEARCH_SUBMIT => model.ready && model.identity.is_some(),
                    PREVIEW => {
                        model.ready && (model.identity.is_none() || model.selected().is_some())
                    }
                    RECORD_FIRST..=8 => {
                        model.identity.is_some()
                            && usize::from(id - RECORD_FIRST) < model.records.len()
                    }
                    PAUSE => model.capture_controls_enabled(),
                    PAUSE_MORE | MENU_PAUSE_15 | MENU_PAUSE_30 | MENU_PAUSE_60
                    | MENU_PAUSE_MANUAL => model.capture_controls_enabled() && model.running,
                    MENU_LOCK => model.identity.is_some(),
                    MENU_RESTART => model.capture_controls_enabled(),
                    EXCLUDE => {
                        model.ready
                            && model.identity.is_some()
                            && model
                                .selected()
                                .is_some_and(|record| !record.process.is_empty())
                    }
                    _ => true,
                };
            if IsWindowEnabled(child).as_bool() != enabled {
                let _ = EnableWindow(child, enabled);
            }
            if id == SEARCH {
                let label = model.labels.get(if model.identity.is_some() {
                    "search"
                } else {
                    "searchLocked"
                });
                if self.cue.borrow().as_str() != label {
                    self.cue.replace(label.to_string());
                    let cue = wide(label);
                    self.cue_supported.set(
                        SendMessageW(
                            child,
                            EM_SETCUEBANNER,
                            WPARAM(1),
                            LPARAM(cue.as_ptr() as isize),
                        )
                        .0 != 0,
                    );
                    let _ = InvalidateRect(child, None, true);
                }
                if model.identity.is_none() && GetWindowTextLengthW(child) > 0 {
                    let _ = SetWindowTextW(child, w!(""));
                }
                if !visible || !enabled {
                    self.composing.set(false);
                }
                continue;
            }
            let label = Zeroizing::new(match id {
                LIGHTWEIGHT => model
                    .labels
                    .get(if model.lightweight {
                        "lightweightOn"
                    } else {
                        "lightweightOff"
                    })
                    .to_string(),
                MORE => model.labels.get("more").to_string(),
                SEARCH_SUBMIT => model.labels.get("searchSubmit").to_string(),
                PREVIEW => model
                    .selected()
                    .map(|record| {
                        format!(
                            "{} · {} · {}",
                            model.labels.get("view"),
                            record.time,
                            record.title
                        )
                    })
                    .unwrap_or_else(|| {
                        model
                            .labels
                            .get(if model.identity.is_none() {
                                "unlock"
                            } else {
                                "empty"
                            })
                            .to_string()
                    }),
                RECORD_FIRST..=8 => model
                    .records
                    .get(usize::from(id - RECORD_FIRST))
                    .map(|record| format!("{} · {}", record.time, record.app))
                    .unwrap_or_default(),
                PAUSE => model.capture_button_label().to_string(),
                PAUSE_MORE => model.labels.get("pauseMore").to_string(),
                MENU_LOCK => model.labels.get("lock").to_string(),
                MENU_SETTINGS => model.labels.get("settings").to_string(),
                MENU_RESTART => model.labels.get("restart").to_string(),
                MENU_QUIT => model.labels.get("quit").to_string(),
                MENU_PAUSE_15 => model.labels.get("pause15").to_string(),
                MENU_PAUSE_30 => model.labels.get("pause30").to_string(),
                MENU_PAUSE_60 => model.labels.get("pause60").to_string(),
                MENU_PAUSE_MANUAL => model.labels.get("pauseManual").to_string(),
                TIMELINE => model.labels.get("timeline").to_string(),
                EXCLUDE => model
                    .labels
                    .get(if model.excluded {
                        "recordApp"
                    } else {
                        "ignoreApp"
                    })
                    .to_string(),
                _ => String::new(),
            });
            let label_wide = Zeroizing::new(wide(&label));
            let mut old = Zeroizing::new(vec![0u16; label_wide.len().max(1024)]);
            let length = GetWindowTextW(child, &mut old) as usize;
            if old[..length] != label_wide[..label_wide.len() - 1] {
                let _ = SetWindowTextW(child, PCWSTR(label_wide.as_ptr()));
            }
        }
        if was_busy && !model.busy {
            let previous = self.pending_focus.take();
            if self.visible.get()
                && GetForegroundWindow() == hwnd
                && IsWindowEnabled(hwnd).as_bool()
            {
                if let Some(child) =
                    previous.and_then(|id| self.controls.borrow().get(&id).copied())
                {
                    if visible_style(child) && IsWindowEnabled(child).as_bool() {
                        let _ = SetFocus(child);
                    }
                }
            }
        }
        self.invalidate(hwnd);
    }

    unsafe fn layout(&self, hwnd: HWND, scale: f32) -> windows::core::Result<()> {
        if self.scale.get() == scale && !self.font.get().is_invalid() {
            return Ok(());
        }
        self.scale.set(scale);
        let font = CreateFontW(
            -(13.0 * scale).round() as i32,
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            DEFAULT_CHARSET.0 as u32,
            OUT_DEFAULT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            CLEARTYPE_QUALITY.0 as u32,
            DEFAULT_PITCH.0 as u32,
            w!("Segoe UI"),
        );
        let old = self.font.replace(font);
        for (&id, &child) in self.controls.borrow().iter() {
            let rect = control_rect(id);
            SetWindowPos(
                child,
                None,
                (rect.x * scale).round() as i32,
                (rect.y * scale).round() as i32,
                (rect.w * scale).round() as i32,
                (rect.h * scale).round() as i32,
                SWP_NOZORDER | SWP_NOACTIVATE,
            )?;
            SendMessageW(child, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(0));
        }
        let width = (WIDTH * scale).round() as i32;
        let height = (HEIGHT * scale).round() as i32;
        let diameter = (24.0 * scale).round() as i32;
        let region = CreateRoundRectRgn(0, 0, width + 1, height + 1, diameter, diameter);
        if !region.is_invalid() && SetWindowRgn(hwnd, region, false) == 0 {
            let _ = DeleteObject(region);
        }
        if !old.is_invalid() {
            let _ = DeleteObject(old);
        }
        if let Some(renderer) = self.renderer.borrow().as_ref() {
            renderer.resize(
                (WIDTH * scale).round() as u32,
                (HEIGHT * scale).round() as u32,
                scale * 96.0,
            )?;
        }
        self.invalidate(hwnd);
        Ok(())
    }

    unsafe fn exclude_search_pixels(&self, dc: HDC) {
        // The EDIT owns its pixels during both painting and background erasure.
        if let Some(edit) = self.controls.borrow().get(&SEARCH) {
            if visible_style(*edit) {
                let r = control_rect(SEARCH);
                let scale = self.scale.get();
                let left = (r.x * scale).round() as i32;
                let top = (r.y * scale).round() as i32;
                let _ = ExcludeClipRect(
                    dc,
                    left,
                    top,
                    left + (r.w * scale).round() as i32,
                    top + (r.h * scale).round() as i32,
                );
            }
        }
    }

    unsafe fn paint(&self, hwnd: HWND, dc: HDC) -> windows::core::Result<()> {
        self.renderer(hwnd)?;
        let saved = SaveDC(dc);
        self.exclude_search_pixels(dc);
        let mut client = RECT::default();
        let model = self.snapshot();
        let controls = self.control_states();
        let menu = self.menu.get();
        let result = GetClientRect(hwnd, &mut client).and_then(|_| {
            self.renderer
                .borrow_mut()
                .as_mut()
                .unwrap()
                .paint(&model, &controls, menu, dc, client)
        });
        if saved != 0 {
            let _ = RestoreDC(dc, saved);
        }
        result
    }

    unsafe fn show(&self, hwnd: HWND, anchor: (i32, i32)) -> windows::core::Result<()> {
        let started = Instant::now();
        self.show_started.set(Some(started));
        self.keyboard_focus.set(false);
        self.menu.set(None);
        let monitor = MonitorFromPoint(
            POINT {
                x: anchor.0,
                y: anchor.1,
            },
            MONITOR_DEFAULTTONEAREST,
        );
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return Err(windows::core::Error::from_win32());
        }
        let mut dpi_x = 96;
        let mut dpi_y = 96;
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        let work = info.rcWork;
        let scale = fit_scale(
            dpi_x as f32 / 96.0,
            (work.left, work.top, work.right, work.bottom),
        );
        let width = (WIDTH * scale).round() as i32;
        let height = (HEIGHT * scale).round() as i32;
        let (x, y) = popup_position(
            anchor,
            (work.left, work.top, work.right, work.bottom),
            (width, height),
        );
        self.layout(hwnd, scale)?;
        SetWindowPos(hwnd, HWND_TOPMOST, x, y, width, height, SWP_NOACTIVATE)?;
        self.visible.set(true);
        (self.action)(Action::Visibility(true));
        self.synchronize(hwnd);
        if !self.snapshot().authenticating {
            let _ = EnableWindow(hwnd, true);
        }
        let dc = GetDC(hwnd);
        let prepared = self.paint(hwnd, dc);
        ReleaseDC(hwnd, dc);
        prepared?;
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        let _ = SetForegroundWindow(hwnd);
        if !self.snapshot().busy {
            self.focus_default();
        } else if !self.snapshot().authenticating {
            let _ = SetFocus(hwnd);
        }
        SetTimer(hwnd, VISIBLE_TIMER, 1000, None);
        let _ = RedrawWindow(
            hwnd,
            None,
            None,
            RDW_INVALIDATE | RDW_ALLCHILDREN | RDW_UPDATENOW,
        );
        tracing::debug!(
            "[TRAY_PANEL] prepared popup controls painted in {:.2} ms",
            started.elapsed().as_secs_f64() * 1000.0
        );
        Ok(())
    }

    unsafe fn hide(&self, hwnd: HWND) {
        if !self.visible.replace(false) {
            return;
        }
        let _ = KillTimer(hwnd, VISIBLE_TIMER);
        let _ = ShowWindow(hwnd, SW_HIDE);
        if let Some(search) = self.controls.borrow().get(&SEARCH) {
            let _ = SetWindowTextW(*search, w!(""));
        }
        self.hover.set(None);
        self.menu.set(None);
        self.composing.set(false);
        (self.action)(Action::Visibility(false));
    }

    unsafe fn focus_default(&self) {
        let id = if self.snapshot().identity.is_some() {
            SEARCH
        } else {
            PREVIEW
        };
        if let Some(child) = self.controls.borrow().get(&id) {
            if IsWindowEnabled(*child).as_bool() {
                let _ = SetFocus(*child);
            }
        }
    }

    unsafe fn restore_owner(&self, hwnd: HWND, completed: bool) {
        if self.snapshot().authenticating {
            return;
        }
        // CNG's modal owner restoration may run on another process/thread.
        // Restore the native owner on its own UI thread on success AND cancel.
        let _ = EnableWindow(hwnd, true);
        self.synchronize(hwnd);
        if self.visible.get() {
            let _ = SetForegroundWindow(hwnd);
            if completed && !self.snapshot().busy {
                self.focus_default();
            } else {
                let _ = SetFocus(hwnd);
            }
        }
    }

    unsafe fn search(&self) {
        let model = self.snapshot();
        if model.busy || model.identity.is_none() || !model.ready {
            return;
        }
        if let Some(edit) = self.controls.borrow().get(&SEARCH) {
            let mut text = Zeroizing::new(vec![0u16; 4097]);
            let length = GetWindowTextW(*edit, &mut text) as usize;
            (self.action)(Action::Search(String::from_utf16_lossy(&text[..length])));
        }
    }

    unsafe fn clicked(&self, hwnd: HWND, id: u16) {
        let model = self.snapshot();
        if model.busy {
            return;
        }
        if self.menu.get().is_some() && !is_menu_control(id) && id != MORE && id != PAUSE_MORE {
            self.close_menu(hwnd, false);
            return;
        }
        let action = match id {
            LIGHTWEIGHT => Action::ToggleLightweight,
            PAUSE | PAUSE_MORE if !model.capture_controls_enabled() => return,
            PAUSE_MORE if !model.running => return,
            MORE | PAUSE_MORE => {
                self.toggle_menu(hwnd, if id == MORE { Menu::More } else { Menu::Pause });
                return;
            }
            SEARCH_SUBMIT => {
                self.search();
                return;
            }
            PREVIEW if model.identity.is_none() && model.ready => Action::Unlock,
            PREVIEW if model.selected().is_some() => Action::Preview,
            RECORD_FIRST..=8 if usize::from(id - RECORD_FIRST) < model.records.len() => {
                Action::Select(usize::from(id - RECORD_FIRST))
            }
            PAUSE if !model.running => Action::Start,
            PAUSE if model.paused => Action::Resume,
            PAUSE => Action::Pause(15),
            TIMELINE => Action::OpenTimeline,
            EXCLUDE if model.identity.is_some() && model.selected().is_some() => {
                Action::ToggleExcluded
            }
            MENU_LOCK if model.identity.is_some() => Action::Lock,
            MENU_SETTINGS => Action::Settings,
            MENU_RESTART if model.capture_controls_enabled() => Action::Restart,
            MENU_QUIT => Action::Quit,
            MENU_PAUSE_15 if model.capture_controls_enabled() && model.running => Action::Pause(15),
            MENU_PAUSE_30 if model.capture_controls_enabled() && model.running => Action::Pause(30),
            MENU_PAUSE_60 if model.capture_controls_enabled() && model.running => Action::Pause(60),
            MENU_PAUSE_MANUAL if model.capture_controls_enabled() && model.running => {
                Action::Pause(0)
            }
            _ => return,
        };
        self.close_menu(hwnd, true);
        (self.action)(action);
    }

    unsafe fn close_menu(&self, hwnd: HWND, restore_focus: bool) -> bool {
        let Some(menu) = self.menu.take() else {
            return false;
        };
        self.hover.set(None);
        self.synchronize(hwnd);
        if restore_focus {
            if let Some(trigger) = self.controls.borrow().get(&menu.trigger()) {
                let _ = SetFocus(*trigger);
            }
        }
        true
    }

    unsafe fn toggle_menu(&self, hwnd: HWND, menu: Menu) {
        if self.menu.get() == Some(menu) {
            self.close_menu(hwnd, true);
            return;
        }
        self.menu.set(Some(menu));
        self.hover.set(None);
        self.synchronize(hwnd);
        for id in menu.controls() {
            if let Some(child) = self.controls.borrow().get(id) {
                let _ = SetWindowPos(
                    *child,
                    HWND_TOP,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
        }
        self.menu_focus(1, false);
    }

    unsafe fn menu_focus(&self, direction: isize, relative: bool) {
        let Some(menu) = self.menu.get() else { return };
        let controls = self.controls.borrow();
        let items: Vec<HWND> = menu
            .controls()
            .iter()
            .filter_map(|id| {
                controls
                    .get(id)
                    .copied()
                    .filter(|child| IsWindowEnabled(*child).as_bool())
            })
            .collect();
        if items.is_empty() {
            return;
        }
        let current = items.iter().position(|child| *child == GetFocus());
        let next = if relative {
            (current.map_or(-1, |index| index as isize) + direction)
                .rem_euclid(items.len() as isize) as usize
        } else if direction < 0 {
            items.len() - 1
        } else {
            0
        };
        let _ = SetFocus(items[next]);
    }

    unsafe fn escape(&self, hwnd: HWND) {
        if !self.close_menu(hwnd, true) {
            self.hide(hwnd);
        }
    }

    unsafe fn renderer(&self, hwnd: HWND) -> windows::core::Result<()> {
        if self.renderer.borrow().is_none() {
            let scale = self.scale.get();
            *self.renderer.borrow_mut() = Some(Renderer::new(
                hwnd,
                (WIDTH * scale).round() as u32,
                (HEIGHT * scale).round() as u32,
                scale * 96.0,
            )?);
        }
        Ok(())
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        // The controls have already been destroyed by WM_NCDESTROY.
        // SAFETY: Context owns these GDI objects and deletes each valid handle
        // once, after the controls that borrowed them have been destroyed.
        unsafe {
            if !self.font.get().is_invalid() {
                let _ = DeleteObject(self.font.get());
            }
            if !self.edit_brush.get().is_invalid() {
                let _ = DeleteObject(self.edit_brush.get());
            }
            if !self.background_brush.get().is_invalid() {
                let _ = DeleteObject(self.background_brush.get());
            }
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
fn handle(value: isize) -> HWND {
    HWND(value as *mut std::ffi::c_void)
}

unsafe fn visible_style(hwnd: HWND) -> bool {
    GetWindowLongPtrW(hwnd, GWL_STYLE) as u32 & WS_VISIBLE.0 != 0
}

fn color_ref(color: windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F) -> COLORREF {
    COLORREF(
        (color.r * 255.0) as u32
            | ((color.g * 255.0) as u32) << 8
            | ((color.b * 255.0) as u32) << 16,
    )
}

unsafe fn apply_frame(hwnd: HWND, dark: bool) {
    let enabled = BOOL::from(dark);
    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_USE_IMMERSIVE_DARK_MODE,
        (&enabled as *const BOOL).cast(),
        std::mem::size_of::<BOOL>() as u32,
    );
    let corner = DWMWCP_ROUND;
    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_WINDOW_CORNER_PREFERENCE,
        (&corner as *const DWM_WINDOW_CORNER_PREFERENCE).cast(),
        std::mem::size_of_val(&corner) as u32,
    );
    let disabled = BOOL::from(true);
    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_TRANSITIONS_FORCEDISABLED,
        (&disabled as *const BOOL).cast(),
        std::mem::size_of::<BOOL>() as u32,
    );
    let no_border = 0xffff_fffeu32;
    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_BORDER_COLOR,
        (&no_border as *const u32).cast(),
        std::mem::size_of::<u32>() as u32,
    );
}

pub(super) fn create(
    model: Arc<Mutex<PanelModel>>,
    action: Box<dyn Fn(Action)>,
    authorized: Box<dyn Fn(ReadIdentity) -> bool>,
) -> std::result::Result<isize, String> {
    // SAFETY: creation runs on the panel's UI thread. The boxed Context moves
    // to window userdata and is reclaimed once by WM_NCDESTROY or an unattached
    // creation failure; all descriptors and text buffers outlive their calls.
    unsafe {
        let instance = HINSTANCE(GetModuleHandleW(None).map_err(|e| e.to_string())?.0);
        let class = w!("CarbonPaperTrayPanel");
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(procedure),
            hInstance: instance,
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW).map_err(|e| e.to_string())?,
            style: CS_HREDRAW | CS_VREDRAW | CS_DROPSHADOW,
            ..Default::default()
        });
        let attached = Rc::new(Cell::new(false));
        let context = Box::into_raw(Box::new(Context {
            model,
            action,
            authorized,
            attached: attached.clone(),
            renderer: RefCell::new(None),
            controls: RefCell::new(BTreeMap::new()),
            scale: Cell::new(1.0),
            font: Cell::new(HFONT::default()),
            edit_brush: Cell::new(HBRUSH::default()),
            background_brush: Cell::new(HBRUSH::default()),
            dark: Cell::new(None),
            hover: Cell::new(None),
            visible: Cell::new(false),
            menu: Cell::new(None),
            composing: Cell::new(false),
            cue_supported: Cell::new(false),
            cue: RefCell::new(String::new()),
            was_busy: Cell::new(false),
            pending_focus: Cell::new(None),
            keyboard_focus: Cell::new(false),
            show_started: Cell::new(None),
        }));
        let hwnd = match CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class,
            w!("CarbonPaper"),
            WS_POPUP,
            0,
            0,
            WIDTH as i32,
            HEIGHT as i32,
            None,
            None,
            instance,
            Some(context.cast()),
        ) {
            Ok(hwnd) => hwnd,
            Err(error) => {
                if !attached.get() {
                    drop(Box::from_raw(context));
                }
                return Err(error.to_string());
            }
        };
        let result = (|| -> windows::core::Result<()> {
            let context = &*context;
            for id in IDS {
                let search = id == SEARCH;
                let style = WS_CHILD
                    | WS_VISIBLE
                    | WS_TABSTOP
                    | WINDOW_STYLE(if search {
                        ES_AUTOHSCROLL as u32
                    } else {
                        BS_OWNERDRAW as u32
                    });
                let child = CreateWindowExW(
                    if search {
                        WINDOW_EX_STYLE::default()
                    } else {
                        WS_EX_TRANSPARENT
                    },
                    if search { w!("EDIT") } else { w!("BUTTON") },
                    w!(""),
                    style,
                    0,
                    0,
                    1,
                    1,
                    hwnd,
                    HMENU(id as *mut std::ffi::c_void),
                    instance,
                    None,
                )?;
                context.controls.borrow_mut().insert(id, child);
                if !SetWindowSubclass(child, Some(control_procedure), id as usize, hwnd.0 as usize)
                    .as_bool()
                {
                    return Err(windows::core::Error::from_win32());
                }
                if search {
                    SendMessageW(child, EM_SETLIMITTEXT, WPARAM(4096), LPARAM(0));
                }
            }
            let scale = GetDpiForWindow(hwnd).max(96) as f32 / 96.0;
            context.scale.set(scale);
            SetWindowPos(
                hwnd,
                None,
                0,
                0,
                (WIDTH * scale).round() as i32,
                (HEIGHT * scale).round() as i32,
                SWP_NOZORDER | SWP_NOACTIVATE,
            )?;
            context.renderer(hwnd)?;
            context.layout(hwnd, scale)?;
            context.synchronize(hwnd);
            // Warm the initial surface and text resources without showing a UI.
            let dc = GetDC(hwnd);
            let painted = context.paint(hwnd, dc);
            ReleaseDC(hwnd, dc);
            painted?;
            Ok(())
        })();
        if let Err(error) = result {
            let _ = DestroyWindow(hwnd);
            return Err(error.to_string());
        }
        Ok(hwnd.0 as isize)
    }
}

pub(super) fn refresh(hwnd: isize) {
    if hwnd != 0 {
        // SAFETY: UPDATE carries no pointers; the owning UI thread handles the
        // refresh and a stale HWND simply fails to receive the message.
        unsafe {
            let _ = PostMessageW(handle(hwnd), UPDATE, WPARAM(0), LPARAM(0));
        }
    }
}
pub(super) fn show(hwnd: isize, anchor: (i32, i32)) -> std::result::Result<(), String> {
    // SAFETY: SHOW copies scalar coordinates to the panel's UI thread; no
    // borrowed memory or HWND ownership crosses the asynchronous message.
    unsafe {
        PostMessageW(
            handle(hwnd),
            SHOW,
            WPARAM(anchor.0 as usize),
            LPARAM(anchor.1 as isize),
        )
        .map_err(|e| e.to_string())
    }
}
pub(super) fn hide(hwnd: isize) {
    if hwnd != 0 {
        // SAFETY: HIDE has scalar arguments and is processed by the owning UI
        // thread; an expired panel handle causes only a failed post.
        unsafe {
            let _ = PostMessageW(handle(hwnd), HIDE, WPARAM(0), LPARAM(0));
        }
    }
}
pub(super) fn focus_after_unlock(hwnd: isize) {
    if hwnd != 0 {
        // SAFETY: FOCUS carries no borrowed data and defers focus changes to
        // the panel's UI thread without transferring its HWND ownership.
        unsafe {
            let _ = PostMessageW(handle(hwnd), FOCUS, WPARAM(0), LPARAM(0));
        }
    }
}
pub(super) fn restore_after_authentication(hwnd: isize) {
    if hwnd != 0 {
        // SAFETY: RESTORE_OWNER carries no pointers; the owning UI thread
        // resolves its own context after receiving the queued notification.
        unsafe {
            let _ = PostMessageW(handle(hwnd), RESTORE_OWNER, WPARAM(0), LPARAM(0));
        }
    }
}
pub(super) fn destroy(hwnd: isize) {
    if hwnd == 0 {
        return;
    }
    // SAFETY: only the owning UI thread destroys the panel. Other threads post
    // a scalar destruction request, and no Rust pointer is retained by Win32.
    unsafe {
        if GetWindowThreadProcessId(handle(hwnd), None) == GetCurrentThreadId() {
            let _ = DestroyWindow(handle(hwnd));
        } else {
            let _ = PostMessageW(handle(hwnd), DESTROY, WPARAM(0), LPARAM(0));
        }
    }
}

unsafe extern "system" fn procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        let creation = &*(lparam.0 as *const CREATESTRUCTW);
        let context = creation.lpCreateParams as *mut Context;
        (*context).attached.set(true);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, context as isize);
    }
    let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Context;
    if pointer.is_null() {
        return DefWindowProcW(hwnd, message, wparam, lparam);
    }
    if message == WM_NCDESTROY {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        let result = DefWindowProcW(hwnd, message, wparam, lparam);
        drop(Box::from_raw(pointer));
        return result;
    }
    let context = &*pointer;
    if message == WM_KEYDOWN {
        context.keyboard_focus.set(true);
    }
    match message {
        WM_NCCALCSIZE if wparam.0 != 0 => return LRESULT(0),
        WM_NCHITTEST => return LRESULT(HTCLIENT as isize),
        WM_ERASEBKGND => {
            let dc = HDC(wparam.0 as *mut std::ffi::c_void);
            let saved = SaveDC(dc);
            context.exclude_search_pixels(dc);
            let mut client = RECT::default();
            let _ = GetClientRect(hwnd, &mut client);
            let brush = context.background_brush.get();
            if !brush.is_invalid() {
                FillRect(dc, &client, brush);
            }
            if saved != 0 {
                let _ = RestoreDC(dc, saved);
            }
            return LRESULT(1);
        }
        WM_CLOSE | HIDE => {
            context.hide(hwnd);
            return LRESULT(0);
        }
        DESTROY => {
            let _ = DestroyWindow(hwnd);
            return LRESULT(0);
        }
        SHOW => {
            if let Err(error) = context.show(hwnd, (wparam.0 as i32, lparam.0 as i32)) {
                tracing::warn!("Native tray popup failed: {error}");
                context.hide(hwnd);
            }
            return LRESULT(0);
        }
        UPDATE => {
            context.synchronize(hwnd);
            return LRESULT(0);
        }
        FOCUS => {
            context.restore_owner(hwnd, true);
            return LRESULT(0);
        }
        RESTORE_OWNER => {
            context.restore_owner(hwnd, false);
            return LRESULT(0);
        }
        WM_ACTIVATE if wparam.0 as u16 == WA_INACTIVE as u16 => {
            if !context.snapshot().authenticating {
                context.hide(hwnd);
            }
        }
        WM_TIMER if wparam.0 == VISIBLE_TIMER => {
            (context.action)(Action::Refresh);
            if !context.snapshot().authenticating && GetForegroundWindow() != hwnd {
                context.hide(hwnd);
            }
            return LRESULT(0);
        }
        WM_DPICHANGED => {
            let dpi_scale = (wparam.0 & 0xffff) as f32 / 96.0;
            let proposed = &*(lparam.0 as *const RECT);
            let monitor = MonitorFromRect(proposed, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            let _ = GetMonitorInfoW(monitor, &mut info);
            let work = (
                info.rcWork.left,
                info.rcWork.top,
                info.rcWork.right,
                info.rcWork.bottom,
            );
            let scale = fit_scale(dpi_scale, work);
            let size = (
                (WIDTH * scale).round() as i32,
                (HEIGHT * scale).round() as i32,
            );
            let position = popup_position((proposed.right, proposed.bottom + 8), work, size);
            let _ = SetWindowPos(
                hwnd,
                None,
                position.0,
                position.1,
                size.0,
                size.1,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            if let Err(error) = context.layout(hwnd, scale) {
                tracing::warn!("Tray DPI resize failed: {error}");
            }
            return LRESULT(0);
        }
        WM_KEYDOWN if wparam.0 == VK_ESCAPE.0 as usize => {
            context.escape(hwnd);
            return LRESULT(0);
        }
        WM_KEYDOWN if wparam.0 == VK_TAB.0 as usize => {
            if let Ok(next) = GetNextDlgTabItem(hwnd, None, GetKeyState(VK_SHIFT.0 as i32) < 0) {
                let _ = SetFocus(next);
            }
            return LRESULT(0);
        }
        WM_COMMAND if (wparam.0 >> 16) as u16 == BN_CLICKED as u16 => {
            context.clicked(hwnd, (wparam.0 & 0xffff) as u16);
            return LRESULT(0);
        }
        WM_LBUTTONDOWN => {
            context.keyboard_focus.set(false);
            context.close_menu(hwnd, false);
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC
            if context
                .controls
                .borrow()
                .get(&SEARCH)
                .is_some_and(|edit| edit.0 as isize == lparam.0) =>
        {
            let p = Palette::new(context.snapshot().dark);
            let dc = HDC(wparam.0 as *mut std::ffi::c_void);
            SetTextColor(
                dc,
                color_ref(if IsWindowEnabled(handle(lparam.0)).as_bool() {
                    p.text
                } else {
                    p.muted
                }),
            );
            SetBkColor(dc, color_ref(p.paper));
            SetBkMode(dc, OPAQUE);
            return LRESULT(context.edit_brush.get().0 as isize);
        }
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut paint);
            let result = context.paint(hwnd, dc);
            let _ = EndPaint(hwnd, &paint);
            if let Err(error) = result {
                *context.renderer.borrow_mut() = None;
                tracing::warn!("Native tray paint will recreate its device: {error}");
            } else if let Some(started) = context.show_started.take() {
                tracing::debug!(
                    "[TRAY_PANEL] prepared popup first paint completed in {:.2} ms",
                    started.elapsed().as_secs_f64() * 1000.0
                );
            }
            return LRESULT(0);
        }
        WM_DRAWITEM => {
            // The parent paints the entire panel in one presentation. These
            // transparent standard buttons retain input and accessibility.
            return LRESULT(1);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, message, wparam, lparam)
}

unsafe extern "system" fn control_procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    parent: usize,
) -> LRESULT {
    let root = HWND(parent as *mut std::ffi::c_void);
    let pointer = GetWindowLongPtrW(root, GWLP_USERDATA) as *const Context;
    if pointer.is_null() {
        return DefSubclassProc(hwnd, message, wparam, lparam);
    }
    let context = &*pointer;
    match message {
        WM_ERASEBKGND if id != SEARCH as usize => return LRESULT(1),
        WM_PAINT if id == SEARCH as usize && !context.cue_supported.get() => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if GetWindowTextLengthW(hwnd) == 0 && !context.composing.get() {
                let model = context.snapshot();
                let color = Palette::new(model.dark).muted;
                let dc = GetDC(hwnd);
                let old_font = SelectObject(dc, context.font.get());
                SetTextColor(
                    dc,
                    COLORREF(
                        (color.r * 255.0) as u32
                            | ((color.g * 255.0) as u32) << 8
                            | ((color.b * 255.0) as u32) << 16,
                    ),
                );
                SetBkMode(dc, TRANSPARENT);
                let mut rect = RECT::default();
                let _ = GetClientRect(hwnd, &mut rect);
                let mut text = wide(&context.cue.borrow());
                DrawTextW(
                    dc,
                    &mut text[..],
                    &mut rect,
                    DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
                );
                SelectObject(dc, old_font);
                ReleaseDC(hwnd, dc);
            }
            return result;
        }
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(hwnd, Some(control_procedure), id);
        }
        WM_GETDLGCODE => {
            return LRESULT(
                DefSubclassProc(hwnd, message, wparam, lparam).0 | DLGC_WANTTAB as isize,
            )
        }
        WM_IME_STARTCOMPOSITION if id == SEARCH as usize => context.composing.set(true),
        WM_IME_ENDCOMPOSITION if id == SEARCH as usize => context.composing.set(false),
        WM_KEYDOWN if !context.composing.get() => {
            context.keyboard_focus.set(true);
            if id != SEARCH as usize {
                context.invalidate(root);
            }
            if wparam.0 == VK_ESCAPE.0 as usize {
                context.escape(root);
                return LRESULT(0);
            }
            if context.menu.get().is_some()
                && (wparam.0 == VK_UP.0 as usize || wparam.0 == VK_DOWN.0 as usize)
            {
                context.menu_focus(if wparam.0 == VK_UP.0 as usize { -1 } else { 1 }, true);
                return LRESULT(0);
            }
            if context.menu.get().is_some()
                && (wparam.0 == VK_HOME.0 as usize || wparam.0 == VK_END.0 as usize)
            {
                context.menu_focus(
                    if wparam.0 == VK_HOME.0 as usize {
                        1
                    } else {
                        -1
                    },
                    false,
                );
                return LRESULT(0);
            }
            if wparam.0 == VK_TAB.0 as usize {
                if let Ok(next) = GetNextDlgTabItem(root, hwnd, GetKeyState(VK_SHIFT.0 as i32) < 0)
                {
                    let _ = SetFocus(next);
                }
                return LRESULT(0);
            }
            if wparam.0 == VK_RETURN.0 as usize {
                if id == SEARCH as usize {
                    context.search();
                } else {
                    context.clicked(root, id as u16);
                }
                return LRESULT(0);
            }
        }
        WM_CHAR
            if !context.composing.get() && (wparam.0 == 13 || wparam.0 == 9 || wparam.0 == 27) =>
        {
            return LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            context.keyboard_focus.set(false);
            context.invalidate(root);
        }
        WM_LBUTTONUP | WM_KEYUP if id != SEARCH as usize => context.invalidate(root),
        WM_MOUSEMOVE if id != SEARCH as usize => {
            if context.hover.replace(Some(id as u16)) != Some(id as u16) {
                context.invalidate(root);
                let mut track = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    ..Default::default()
                };
                let _ = TrackMouseEvent(&mut track);
            }
        }
        WM_MOUSELEAVE => {
            if context.hover.get() == Some(id as u16) {
                context.hover.set(None);
                context.invalidate(root);
            }
        }
        WM_SETFOCUS | WM_KILLFOCUS => context.invalidate(root),
        _ => {}
    }
    DefSubclassProc(hwnd, message, wparam, lparam)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestWindow(isize);
    impl Drop for TestWindow {
        fn drop(&mut self) {
            destroy(self.0);
        }
    }

    #[test]
    fn power_saving_blocks_capture_clicks_and_restores_controls_when_cleared() {
        // Use real hidden controls and painting without changing system power,
        // accessing storage, or starting capture.
        for (language, dark, power_label) in [
            ("zh-CN", true, "因节电模式暂停"),
            ("en", false, "Paused to save power"),
        ] {
            for (running, paused, restored_label) in [
                (false, false, "start"),
                (true, true, "resume"),
                (true, false, "pause15"),
            ] {
                let labels = Labels(
                    ["powerSaving", "start", "resume", "pause15"]
                        .into_iter()
                        .map(|key| (key, crate::i18n::t(language, &format!("trayPanel.{key}"))))
                        .collect(),
                );
                let model = Arc::new(Mutex::new(PanelModel {
                    labels,
                    dark,
                    ready: true,
                    controls_enabled: true,
                    power_saving: true,
                    running,
                    paused,
                    status: "powerSaving",
                    ..Default::default()
                }));
                let actions = Arc::new(Mutex::new(Vec::new()));
                let received = actions.clone();
                let window = TestWindow(
                    create(
                        model.clone(),
                        Box::new(move |action| received.lock().unwrap().push(action)),
                        Box::new(|_| true),
                    )
                    .unwrap(),
                );
                // SAFETY: the fixture owns the parent HWND and all its controls
                // on this UI thread; buffers outlive synchronous queries/messages.
                unsafe {
                    let root = handle(window.0);
                    let primary = GetDlgItem(root, PAUSE as i32).unwrap();
                    let mut text = [0u16; 128];
                    let length = GetWindowTextW(primary, &mut text) as usize;
                    assert_eq!(String::from_utf16_lossy(&text[..length]), power_label);

                    for id in [
                        PAUSE,
                        PAUSE_MORE,
                        MENU_RESTART,
                        MENU_PAUSE_15,
                        MENU_PAUSE_30,
                        MENU_PAUSE_60,
                        MENU_PAUSE_MANUAL,
                    ] {
                        let child = GetDlgItem(root, id as i32).unwrap();
                        assert!(!IsWindowEnabled(child).as_bool(), "control {id}");
                        // A queued notification must also respect the policy.
                        SendMessageW(
                            root,
                            WM_COMMAND,
                            WPARAM(id as usize),
                            LPARAM(child.0 as isize),
                        );
                    }
                    assert!(actions.lock().unwrap().is_empty());

                    {
                        let mut state = model.lock().unwrap();
                        state.power_saving = false;
                        state.status = if !running {
                            "stopped"
                        } else if paused {
                            "paused"
                        } else {
                            "running"
                        };
                    }
                    SendMessageW(root, UPDATE, WPARAM(0), LPARAM(0));
                    assert!(IsWindowEnabled(primary).as_bool());
                    assert_eq!(
                        IsWindowEnabled(GetDlgItem(root, PAUSE_MORE as i32).unwrap()).as_bool(),
                        running
                    );
                    assert!(
                        IsWindowEnabled(GetDlgItem(root, MENU_RESTART as i32).unwrap()).as_bool()
                    );
                    let length = GetWindowTextW(primary, &mut text) as usize;
                    assert_eq!(
                        String::from_utf16_lossy(&text[..length]),
                        crate::i18n::t(language, &format!("trayPanel.{restored_label}"))
                    );
                    SendMessageW(primary, BM_CLICK, WPARAM(0), LPARAM(0));
                    let actions = actions.lock().unwrap();
                    assert_eq!(actions.len(), 1);
                    assert!(matches!(
                        (running, paused, &actions[0]),
                        (false, _, Action::Start)
                            | (true, true, Action::Resume)
                            | (true, false, Action::Pause(15))
                    ));
                }
            }
        }
    }

    #[test]
    fn modal_unlock_restores_input_and_outside_dismissal_on_success_and_cancel() {
        // Exercise the real HWND/controller without showing a window or
        // invoking Windows Hello, accessing storage, or touching the monitor.
        for verified in [false, true] {
            let model = Arc::new(Mutex::new(PanelModel {
                ready: true,
                controls_enabled: true,
                running: true,
                status: "running",
                ..Default::default()
            }));
            let actions = Arc::new(Mutex::new(Vec::new()));
            let received = actions.clone();
            let window = TestWindow(
                create(
                    model.clone(),
                    Box::new(move |action| received.lock().unwrap().push(action)),
                    Box::new(|_| true),
                )
                .unwrap(),
            );
            // SAFETY: TestWindow keeps the HWND, userdata Context and controls
            // live on this UI thread; synchronous messages cannot outlive them.
            unsafe {
                let root = handle(window.0);
                let context = &*(GetWindowLongPtrW(root, GWLP_USERDATA) as *const Context);
                let preview = GetDlgItem(root, PREVIEW as i32).unwrap();
                let edit = GetDlgItem(root, SEARCH as i32).unwrap();

                {
                    let mut state = model.lock().unwrap();
                    state.busy = true;
                    state.authenticating = true;
                }
                SendMessageW(root, UPDATE, WPARAM(0), LPARAM(0));
                assert_eq!(
                    GetWindowLongPtrW(root, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0,
                    0,
                    "the panel must yield its topmost placement to the credential dialog"
                );
                context.visible.set(true);
                let _ = EnableWindow(root, false);
                SendMessageW(root, WM_ACTIVATE, WPARAM(WA_INACTIVE as usize), LPARAM(0));
                assert!(
                    context.visible.get(),
                    "the credential dialog owns focus while verification is running"
                );
                assert!(!IsWindowEnabled(root).as_bool());

                model.lock().unwrap().authenticating = false;
                context.visible.set(false);
                SendMessageW(root, RESTORE_OWNER, WPARAM(0), LPARAM(0));
                assert_ne!(
                    GetWindowLongPtrW(root, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0,
                    0,
                    "the popup's topmost placement must return after authentication"
                );
                assert!(
                    IsWindowEnabled(root).as_bool(),
                    "CNG's disabled owner must be restored before service completion"
                );
                assert!(
                    !IsWindowEnabled(preview).as_bool(),
                    "protected work still observes the pending operation"
                );
                context.visible.set(true);
                SendMessageW(root, WM_ACTIVATE, WPARAM(WA_INACTIVE as usize), LPARAM(0));
                assert!(
                    !context.visible.get(),
                    "outside dismissal must work during post-verification work"
                );

                {
                    let mut state = model.lock().unwrap();
                    state.busy = false;
                    if verified {
                        state.identity = Some(ReadIdentity {
                            session: Instant::now(),
                            database: 1,
                        });
                    }
                }
                SendMessageW(root, FOCUS, WPARAM(0), LPARAM(0));
                assert!(
                    !context.visible.get(),
                    "late completion must not reopen a dismissed panel"
                );
                assert!(IsWindowEnabled(root).as_bool());
                if verified {
                    assert!(IsWindowEnabled(edit).as_bool());
                    SetWindowTextW(edit, w!("sample query")).unwrap();
                    SendMessageW(edit, WM_KEYDOWN, WPARAM(VK_RETURN.0 as usize), LPARAM(0));
                    assert!(actions.lock().unwrap().iter().any(
                        |action| matches!(action, Action::Search(query) if query == "sample query")
                    ));
                } else {
                    assert!(IsWindowEnabled(preview).as_bool());
                    assert!(!IsWindowEnabled(edit).as_bool());
                    SendMessageW(preview, BM_CLICK, WPARAM(0), LPARAM(0));
                    assert!(actions
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|action| matches!(action, Action::Unlock)));
                }

                context.visible.set(true);
                SendMessageW(
                    GetDlgItem(root, MORE as i32).unwrap(),
                    BM_CLICK,
                    WPARAM(0),
                    LPARAM(0),
                );
                assert_eq!(context.menu.get(), Some(Menu::More));
                assert!(
                    !visible_style(edit),
                    "the popup must cover the native search control"
                );
                SendMessageW(root, WM_KEYDOWN, WPARAM(VK_ESCAPE.0 as usize), LPARAM(0));
                assert!(context.menu.get().is_none());
                assert!(
                    context.visible.get(),
                    "Escape dismisses the menu before the panel"
                );
                assert!(visible_style(edit));
                SendMessageW(
                    GetDlgItem(root, PAUSE_MORE as i32).unwrap(),
                    BM_CLICK,
                    WPARAM(0),
                    LPARAM(0),
                );
                assert_eq!(context.menu.get(), Some(Menu::Pause));
                SendMessageW(root, WM_ACTIVATE, WPARAM(WA_INACTIVE as usize), LPARAM(0));
                assert!(
                    !context.visible.get(),
                    "an open custom menu must not prevent outside dismissal"
                );
                assert!(context.menu.get().is_none());
            }
        }
    }
}
