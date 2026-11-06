//! Restore the software-rendered window after Windows discards its surface.

#[cfg(target_os = "windows")]
pub fn install(window: &slint::Window) {
    use slint::winit_030::{EventResult, WinitWindowAccessor, winit::event::WindowEvent};

    let mut surface_invalid = false;
    window.on_winit_window_event(move |window, event| {
        match event {
            WindowEvent::Occluded(true) => surface_invalid = true,
            WindowEvent::Resized(size) if size.width == 0 || size.height == 0 => {
                surface_invalid = true;
            }
            WindowEvent::Resized(_) | WindowEvent::Occluded(false) | WindowEvent::Focused(true)
                if surface_invalid =>
            {
                // Bypass Slint's pending-frame flag: a frame queued while minimized
                // may still be pending, although Windows discarded its presentation.
                window.with_winit_window(|native| native.request_redraw());
            }
            WindowEvent::RedrawRequested => {
                let minimized = window
                    .with_winit_window(|native| native.is_minimized().unwrap_or(false))
                    .unwrap_or(false);
                if minimized {
                    surface_invalid = true;
                    return EventResult::PreventDefault;
                }
                if surface_invalid {
                    invalidate_surface(window);
                    surface_invalid = false;
                }
            }
            _ => {}
        }
        EventResult::Propagate
    });
}

#[cfg(target_os = "windows")]
fn invalidate_surface(window: &slint::Window) {
    use i_slint_core::{lengths::LogicalRect, window::WindowInner};

    // request_redraw() alone retains the partial-render cache. Slint 1.18.1 has
    // no public full-invalidation API, so keep this version-pinned call here.
    // Invalidate just before the first visible frame, never on a timer.
    let size = window.size().to_logical(window.scale_factor());
    WindowInner::from_pub(window)
        .window_adapter()
        .renderer()
        .mark_dirty_region(LogicalRect::from_size([size.width, size.height].into()).into());
}

#[cfg(not(target_os = "windows"))]
pub fn install(_window: &slint::Window) {}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;
    use slint::platform::{
        Platform, WindowAdapter,
        software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
    };
    use std::rc::Rc;

    #[test]
    fn discarded_surface_is_fully_repainted_without_changing_ui_properties() {
        struct Backend(Rc<MinimalSoftwareWindow>);
        impl Platform for Backend {
            fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
                Ok(self.0.clone())
            }
        }
        slint::slint! {
            export component SurfaceCheck inherits Window {
                width: 160px; height: 100px; background: #ddeeff;
                Rectangle { x: 20px; y: 10px; width: 55px; height: 75px; background: #224466; }
            }
        }
        let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
        slint::platform::set_platform(Box::new(Backend(window.clone()))).unwrap();
        let ui = SurfaceCheck::new().unwrap();
        ui.show().unwrap();
        ui.window().set_size(slint::PhysicalSize::new(160, 100));
        let mut pixels = vec![slint::Rgb8Pixel::default(); 160 * 100];
        window.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, 160);
        });
        let expected = pixels.clone();
        pixels.fill(slint::Rgb8Pixel::default());
        ui.window().request_redraw();
        window.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, 160);
        });
        assert_ne!(
            pixels, expected,
            "a redraw request alone must reproduce the stale-cache problem"
        );
        invalidate_surface(ui.window());
        ui.window().request_redraw();
        window.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, 160);
        });
        assert_eq!(
            pixels, expected,
            "the restored frame must repaint even unchanged areas"
        );
    }
}
