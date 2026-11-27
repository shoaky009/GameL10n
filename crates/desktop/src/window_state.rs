//! Local window preferences, independent of the selected game data directory.

use gamel10n_core::projects::{APP_DATA_DIR_NAME, ENV_LOCAL_APP_DATA};
use gamel10n_engines::protocol::GAME_ROOT_DIR;
use gamel10n_translation::files::{FileError, read_json, write_json};
use serde::{Deserialize, Serialize};
use slint::winit_030::{WinitWindowAccessor, winit::event::WindowEvent};
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
};

const UI_STATE_FILENAME: &str = "ui-state.json";

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct Geometry {
    x: i32,
    y: i32,
    // Logical client dimensions keep the interface size stable across DPI changes.
    width: f64,
    height: f64,
    maximized: bool,
}

impl Geometry {
    fn valid(&self) -> bool {
        self.width.is_finite()
            && self.height.is_finite()
            && self.width > 0.0
            && self.height > 0.0
            && self.width <= f64::from(f32::MAX)
            && self.height <= f64::from(f32::MAX)
    }

    fn apply(&self, window: &slint::Window) {
        window.set_position(slint::PhysicalPosition::new(self.x, self.y));
        window.set_size(slint::LogicalSize::new(
            self.width as f32,
            self.height as f32,
        ));
    }

    fn fit(&self, monitors: &[Monitor], primary: usize, frame: [f64; 2]) -> Self {
        let overlap = |monitor: &Monitor| {
            let right = f64::from(self.x) + (self.width + frame[0]) * monitor.scale;
            let bottom = f64::from(self.y) + (self.height + frame[1]) * monitor.scale;
            (right.min(monitor.right()) - f64::from(self.x.max(monitor.x))).max(0.0)
                * (bottom.min(monitor.bottom()) - f64::from(self.y.max(monitor.y))).max(0.0)
        };
        let Some(monitor) = monitors
            .iter()
            .max_by(|left, right| overlap(left).total_cmp(&overlap(right)))
        else {
            return *self;
        };
        let visible = overlap(monitor) > 0.0;
        let monitor = if visible {
            monitor
        } else {
            monitors.get(primary).unwrap_or(monitor)
        };
        let width = self
            .width
            .min((f64::from(monitor.width) / monitor.scale - frame[0]).max(1.0));
        let height = self
            .height
            .min((f64::from(monitor.height) / monitor.scale - frame[1]).max(1.0));
        let outer_width = (width + frame[0]) * monitor.scale;
        let outer_height = (height + frame[1]) * monitor.scale;
        let x = if visible {
            f64::from(self.x)
        } else {
            f64::from(monitor.x) + (f64::from(monitor.width) - outer_width) / 2.0
        };
        let y = if visible {
            f64::from(self.y)
        } else {
            f64::from(monitor.y) + (f64::from(monitor.height) - outer_height) / 2.0
        };
        Self {
            x: x.clamp(
                f64::from(monitor.x),
                (monitor.right() - outer_width).max(f64::from(monitor.x)),
            )
            .round() as i32,
            y: y.clamp(
                f64::from(monitor.y),
                (monitor.bottom() - outer_height).max(f64::from(monitor.y)),
            )
            .round() as i32,
            width,
            height,
            ..*self
        }
    }
}

struct Monitor {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    scale: f64,
}

impl Monitor {
    fn right(&self) -> f64 {
        f64::from(self.x) + f64::from(self.width)
    }
    fn bottom(&self) -> f64 {
        f64::from(self.y) + f64::from(self.height)
    }
}

#[derive(Clone)]
pub struct WindowState {
    path: PathBuf,
    geometry: Rc<RefCell<Option<Geometry>>>,
}

impl WindowState {
    pub fn install(window: &slint::Window) -> Self {
        let path = std::env::var_os(ENV_LOCAL_APP_DATA)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(GAME_ROOT_DIR))
            .join(APP_DATA_DIR_NAME)
            .join(UI_STATE_FILENAME);
        Self::at(window, path)
    }

    fn at(window: &slint::Window, path: PathBuf) -> Self {
        let saved = load(&path);
        let geometry = Rc::new(RefCell::new(saved));
        let state = geometry.clone();
        let mut restored = false;
        let mut redraw = crate::window_redraw::event_filter();
        // Slint accepts one native event filter; geometry and surface recovery share it.
        window.on_winit_window_event(move |window, event| {
            if !restored {
                restored = window
                    .with_winit_window(|native| {
                        let mut geometry = state.borrow_mut();
                        let current = sample(native);
                        let saved = geometry.unwrap_or(current);
                        let handles: Vec<_> = native.available_monitors().collect();
                        let primary = handles
                            .iter()
                            .position(|monitor| Some(monitor) == native.primary_monitor().as_ref())
                            .unwrap_or(0);
                        let monitors: Vec<_> = handles
                            .iter()
                            .map(|monitor| Monitor {
                                x: monitor.position().x,
                                y: monitor.position().y,
                                width: monitor.size().width,
                                height: monitor.size().height,
                                scale: monitor.scale_factor(),
                            })
                            .collect();
                        let inner = native.inner_size();
                        let outer = native.outer_size();
                        let scale = native.scale_factor();
                        let frame = [
                            f64::from(outer.width.saturating_sub(inner.width)) / scale,
                            f64::from(outer.height.saturating_sub(inner.height)) / scale,
                        ];
                        let fitted = saved.fit(&monitors, primary, frame);
                        *geometry = Some(fitted);
                        fitted
                    })
                    .map(|geometry| {
                        geometry.apply(window);
                        window.set_maximized(geometry.maximized);
                    })
                    .is_some();
            } else if matches!(event, WindowEvent::RedrawRequested) {
                window.with_winit_window(|native| {
                    record(
                        &mut state.borrow_mut(),
                        sample(native),
                        native.is_minimized().unwrap_or(false) || native.fullscreen().is_some(),
                    );
                });
            }
            if matches!(event, WindowEvent::Moved(_) | WindowEvent::Resized(_)) {
                // Windows reports movement before the maximize/minimize transition completes.
                // Read geometry on the resulting redraw, after those notifications settle.
                window.with_winit_window(|native| native.request_redraw());
            }
            redraw(window, event)
        });
        Self { path, geometry }
    }

    pub fn save(&self) -> Result<(), FileError> {
        if let Some(geometry) = *self.geometry.borrow() {
            write_json(&self.path, &geometry)?;
        }
        Ok(())
    }

    pub fn capture(&self, window: &slint::Window) {
        window.with_winit_window(|native| {
            record(
                &mut self.geometry.borrow_mut(),
                sample(native),
                native.is_minimized().unwrap_or(false) || native.fullscreen().is_some(),
            );
        });
    }
}

fn load(path: &Path) -> Option<Geometry> {
    read_json::<Geometry>(path).ok().filter(Geometry::valid)
}

fn sample(window: &slint::winit_030::winit::window::Window) -> Geometry {
    let position = window.outer_position().unwrap_or_default();
    let size = window.inner_size();
    let scale = window.scale_factor();
    Geometry {
        x: position.x,
        y: position.y,
        width: f64::from(size.width) / scale,
        height: f64::from(size.height) / scale,
        maximized: window.is_maximized(),
    }
}

fn record(saved: &mut Option<Geometry>, current: Geometry, minimized: bool) {
    if minimized || !current.valid() {
        return;
    }
    if current.maximized {
        if let Some(saved) = saved {
            saved.maximized = true;
        }
    } else {
        *saved = Some(current);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geometry() -> Geometry {
        Geometry {
            x: 120,
            y: 80,
            width: 1420.0,
            height: 900.0,
            maximized: false,
        }
    }

    fn monitor(x: i32, y: i32, width: u32, height: u32, scale: f64) -> Monitor {
        Monitor {
            x,
            y,
            width,
            height,
            scale,
        }
    }

    #[test]
    fn visible_geometry_and_negative_monitor_positions_are_preserved() {
        let saved = Geometry {
            x: -1800,
            y: 100,
            ..geometry()
        };
        let monitors = [
            monitor(0, 0, 2560, 1440, 1.0),
            monitor(-1920, 0, 1920, 1080, 1.0),
        ];
        assert_eq!(saved.fit(&monitors, 0, [16.0, 39.0]), saved);
    }

    #[test]
    fn disconnected_monitor_returns_the_window_to_the_primary_monitor() {
        let saved = Geometry {
            x: 4000,
            y: -1500,
            ..geometry()
        };
        let fitted = saved.fit(&[monitor(0, 0, 1920, 1080, 1.0)], 0, [16.0, 39.0]);
        assert_eq!(
            (fitted.x, fitted.y, fitted.width, fitted.height),
            (242, 71, 1420.0, 900.0)
        );
    }

    #[test]
    fn higher_dpi_and_smaller_resolution_limit_the_restored_geometry() {
        let saved = Geometry {
            x: 1700,
            y: 900,
            ..geometry()
        };
        let fitted = saved.fit(&[monitor(0, 0, 1920, 1080, 1.5)], 0, [16.0, 40.0]);
        assert_eq!(
            (fitted.x, fitted.y, fitted.width, fitted.height),
            (0, 0, 1264.0, 680.0)
        );
    }

    #[test]
    fn maximized_and_minimized_windows_keep_the_normal_geometry() {
        let normal = geometry();
        let mut saved = Some(normal);
        record(
            &mut saved,
            Geometry {
                x: 0,
                y: 0,
                width: 1920.0,
                height: 1080.0,
                maximized: true,
            },
            false,
        );
        record(
            &mut saved,
            Geometry {
                x: -32000,
                y: -32000,
                width: 0.0,
                height: 0.0,
                maximized: false,
            },
            true,
        );
        assert_eq!(
            saved,
            Some(Geometry {
                maximized: true,
                ..normal
            })
        );
        record(&mut saved, normal, false);
        assert_eq!(saved, Some(normal));
    }

    #[test]
    fn window_state_round_trips_without_other_settings() {
        let root = tempfile::tempdir().unwrap();
        let state = WindowState {
            path: root.path().join("ui-state.json"),
            geometry: Rc::new(RefCell::new(Some(geometry()))),
        };
        state.save().unwrap();
        assert_eq!(read_json::<Geometry>(&state.path).unwrap(), geometry());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn missing_corrupt_and_invalid_window_state_use_defaults() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("ui-state.json");
        assert!(load(&path).is_none());
        std::fs::write(&path, b"{\"x\":120,").unwrap();
        assert!(load(&path).is_none());
        for width in [0.0, -1.0, f64::MAX] {
            write_json(
                &path,
                &Geometry {
                    width,
                    ..geometry()
                },
            )
            .unwrap();
            assert!(load(&path).is_none());
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "Requires the Windows desktop and Winit backend."]
    fn native_hidden_windows_restore_geometry_and_ignore_corrupt_state() {
        use slint::{
            ComponentHandle, winit_030::winit::platform::windows::EventLoopBuilderExtWindows,
        };
        let mut builder = slint::winit_030::winit::event_loop::EventLoop::with_user_event();
        builder.with_any_thread(true);
        slint::BackendSelector::new()
            .backend_name("winit".into())
            .with_winit_event_loop_builder(builder)
            .select()
            .unwrap();
        slint::slint! {
            export component StateCheck inherits Window {
                preferred-width: 1420px; preferred-height: 900px;
            }
        }
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("ui-state.json");
        let saved = Geometry {
            x: -30000,
            y: -30000,
            width: 900.0,
            height: 600.0,
            maximized: false,
        };
        write_json(&path, &saved).unwrap();
        let ui = StateCheck::new().unwrap();
        let state = WindowState::at(ui.window(), path.clone());
        // Queue native creation before the event loop, with visibility already cleared.
        ui.show().unwrap();
        ui.hide().unwrap();
        let corrupt_path = root.path().join("corrupt-state.json");
        std::fs::write(&corrupt_path, b"{\"width\":").unwrap();
        let fallback_ui = StateCheck::new().unwrap();
        let fallback_state = WindowState::at(fallback_ui.window(), corrupt_path.clone());
        assert!(fallback_state.geometry.borrow().is_none());
        fallback_ui.show().unwrap();
        fallback_ui.hide().unwrap();
        let completed = Rc::new(std::cell::Cell::new(false));
        let passed = completed.clone();
        let timeout = slint::Timer::default();
        timeout.start(
            slint::TimerMode::SingleShot,
            std::time::Duration::from_secs(5),
            || {
                slint::quit_event_loop().unwrap();
            },
        );
        let _task = slint::spawn_local(async move {
            let native = ui.window().winit_window().await.unwrap();
            let fallback_native = fallback_ui.window().winit_window().await.unwrap();
            assert_eq!(native.is_visible(), Some(false));
            slint::Timer::single_shot(
                std::time::Duration::from_millis(100),
                move || {
                    let actual = sample(&native);
                    let monitors: Vec<_> = native.available_monitors().collect();
                    assert!(
                        monitors.iter().any(|monitor| {
                            let position = monitor.position();
                            let size = monitor.size();
                            actual.x >= position.x
                                && actual.y >= position.y
                                && i64::from(actual.x)
                                    < i64::from(position.x) + i64::from(size.width)
                                && i64::from(actual.y)
                                    < i64::from(position.y) + i64::from(size.height)
                        }),
                        "restored window must be on a connected monitor: {actual:?}"
                    );
                    assert!((actual.width - saved.width).abs() <= 1.0,
                        "logical width mismatch: {actual:?}; saved: {saved:?}; tracked: {:?}; monitors: {:?}",
                        state.geometry.borrow(), monitors.iter().map(|monitor| (monitor.size(), monitor.scale_factor())).collect::<Vec<_>>());
                    assert!((actual.height - saved.height).abs() <= 1.0, "logical height mismatch: {actual:?}; saved: {saved:?}");
                    assert!(!actual.maximized);
                    assert_eq!(native.is_visible(), Some(false));
                    state.capture(ui.window());
                    state.save().unwrap();
                    assert_eq!(load(&path), Some(actual));
                    assert!(sample(&fallback_native).valid());
                    assert_eq!(fallback_native.is_visible(), Some(false));
                    fallback_state.capture(fallback_ui.window());
                    fallback_state.save().unwrap();
                    assert!(load(&corrupt_path).is_some());
                    drop(native);
                    drop(fallback_native);
                    drop(ui);
                    drop(fallback_ui);
                    passed.set(true);
                    slint::quit_event_loop().unwrap();
                },
            );
        })
        .unwrap();
        slint::run_event_loop().unwrap();
        assert!(completed.get(), "native window restoration timed out");
    }
}
