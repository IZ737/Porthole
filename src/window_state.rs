//! Remembering the window's size, position and maximised state between
//! launches, in `<APP_DATA_DIR>/porthole-window.json`.

use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const FILE_NAME: &str = "porthole-window.json";

/// The window's normal (not maximised, not minimised) bounds, plus whether
/// it was maximised.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowState {
    /// Outer position in physical pixels. `None` where the platform does
    /// not report it (Wayland).
    pub x: Option<i32>,
    pub y: Option<i32>,
    /// Inner size in logical pixels.
    pub width: f64,
    pub height: f64,
    pub maximized: bool,
}

impl WindowState {
    /// Read the saved state; `None` if it is missing or unusable.
    pub fn load(path: &Path) -> Option<Self> {
        let text = fs::read_to_string(path).ok()?;
        let state: Self = serde_json::from_str(&text).ok()?;
        let sane = |v: f64| v.is_finite() && (100.0..=100_000.0).contains(&v);
        (sane(state.width) && sane(state.height)).then_some(state)
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        fs::write(path, json)
    }
}

/// A monitor's area in physical pixels: x, y, width, height.
pub type Rect = (i32, i32, u32, u32);

/// Whether a window at `pos` of physical `size` would be usably on screen:
/// enough of it overlaps some monitor that the user can grab it.
pub fn is_on_screen(pos: (i32, i32), size: (u32, u32), monitors: &[Rect]) -> bool {
    let need_w = i64::from(size.0.min(100));
    let need_h = i64::from(size.1.min(50));
    monitors.iter().any(|&(mx, my, mw, mh)| {
        let (wx0, wy0) = (i64::from(pos.0), i64::from(pos.1));
        let (wx1, wy1) = (wx0 + i64::from(size.0), wy0 + i64::from(size.1));
        let (mx0, my0) = (i64::from(mx), i64::from(my));
        let (mx1, my1) = (mx0 + i64::from(mw), my0 + i64::from(mh));
        let w = wx1.min(mx1) - wx0.max(mx0);
        let h = wy1.min(my1) - wy0.max(my0);
        // The top edge (title bar) has to be on this monitor too.
        w >= need_w && h >= need_h && wy0 >= my0 && wy0 < my1
    })
}

/// Top-left position that centres `size` on `monitor`.
pub fn centered(size: (u32, u32), monitor: Rect) -> (i32, i32) {
    let (mx, my, mw, mh) = monitor;
    let x = i64::from(mx) + (i64::from(mw) - i64::from(size.0)) / 2;
    let y = i64::from(my) + (i64::from(mh) - i64::from(size.1)) / 2;
    (x.max(i64::from(mx)) as i32, y.max(i64::from(my)) as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRIMARY: Rect = (0, 0, 1920, 1080);
    const LEFT: Rect = (-1280, 0, 1280, 1024);

    #[test]
    fn on_screen_checks() {
        assert!(is_on_screen((100, 100), (800, 600), &[PRIMARY]));
        assert!(is_on_screen((-1000, 100), (800, 600), &[PRIMARY, LEFT]));
        // The monitor on the left was unplugged.
        assert!(!is_on_screen((-1000, 100), (800, 600), &[PRIMARY]));
        // Mostly off the right edge: only 20 px visible.
        assert!(!is_on_screen((1900, 100), (800, 600), &[PRIMARY]));
        // Title bar above the top of the screen.
        assert!(!is_on_screen((100, -300), (800, 600), &[PRIMARY]));
        assert!(!is_on_screen((100, 100), (800, 600), &[]));
    }

    #[test]
    fn centres() {
        assert_eq!(centered((800, 600), PRIMARY), (560, 240));
        assert_eq!(centered((800, 600), LEFT), (-1040, 212));
        // Larger than the monitor: pin to its corner.
        assert_eq!(centered((4000, 3000), PRIMARY), (0, 0));
    }

    #[test]
    fn save_and_load() {
        let path = std::env::temp_dir().join(format!("porthole-ws-{}.json", std::process::id()));
        let state = WindowState {
            x: Some(-5),
            y: None,
            width: 1024.0,
            height: 700.5,
            maximized: true,
        };
        state.save(&path).unwrap();
        assert_eq!(WindowState::load(&path), Some(state));
        fs::write(
            &path,
            r#"{"x":1,"y":2,"width":0,"height":5,"maximized":false}"#,
        )
        .unwrap();
        assert_eq!(WindowState::load(&path), None);
        fs::write(&path, "not json").unwrap();
        assert_eq!(WindowState::load(&path), None);
        let _ = fs::remove_file(&path);
    }
}
