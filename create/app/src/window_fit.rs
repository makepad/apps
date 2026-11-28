//! Windows that show a picture at its own pixels: the share pop-out and the chat's picture window.
use makepad_widgets::*;

/// The inner size, in layout points, that shows `pixels` at one device pixel each, capped to
/// `available` (the display's work area less the window's frame).
fn exact_pixel_size(pixels: DVec2, dpi: f64, available: DVec2) -> DVec2 {
    let size = pixels / dpi;
    dvec2(size.x.min(available.x), size.y.min(available.y))
}

/// Sizes `window` so it shows `pixels` at exact pixels, capped to the work area of the display
/// it is on. `chrome` is what the window draws around the picture inside its own size, such as
/// an app-drawn caption bar. Returns false, leaving the window as it is, while it cannot be
/// sized: not created yet, fullscreen, maximised, or with no DPI yet.
pub(crate) fn fit_window_to_pixels(cx: &mut Cx, window: &WindowRef, pixels: DVec2, chrome: DVec2) -> bool {
    let Some(id) = window.window_id() else { return false };
    let native = &cx.windows[id];
    let geom = &native.window_geom;
    if !native.is_created || geom.is_fullscreen || geom.is_maximized || geom.dpi_factor <= 0. { return false }
    // Screen rectangles and window positions share OS units. Resize takes layout points.
    let units = match cx.os_type() {
        OsType::Windows | OsType::LinuxWindow(_) => geom.dpi_factor,
        OsType::LinuxDirect => 1.,
        _ => native.layout_points_to_native_points(1.),
    };
    let center = geom.position + geom.outer_size * (0.5 * units);
    let displays = makepad_platform::screens();
    let display = displays.iter().find(|s| s.bounds.contains(center))
        .or_else(|| displays.iter().find(|s| s.bounds.contains(geom.position)))
        .or_else(|| displays.iter().find(|s| s.is_primary));
    let available = display.map_or(dvec2(f64::INFINITY, f64::INFINITY), |s|
        s.work_area.size / units - (geom.outer_size - geom.inner_size));
    let size = exact_pixel_size(pixels, geom.dpi_factor, available - chrome) + chrome;
    if size != geom.inner_size { window.resize(cx, size); }
    true
}

#[cfg(test)]
#[test]
fn exact_pixels_fit_the_display() {
    let frame = dvec2(1694., 1058.);
    let screen = dvec2(1920., 1200.);
    assert_eq!(exact_pixel_size(frame, 1., screen), frame, "at dpi 1 a window is the picture's pixels");
    assert_eq!(exact_pixel_size(frame, 2., screen), dvec2(847., 529.), "at dpi 2 a point holds two pixels");
    assert_eq!(exact_pixel_size(frame, 2., dvec2(700., 500.)), dvec2(700., 500.), "a small display caps the window");
}
