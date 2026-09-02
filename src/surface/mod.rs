//! Platform surfaces: a GPU surface positioned *behind* the webview.
//!
//! Each backend does exactly two things:
//!   1. make the webview's background transparent, so what is behind it shows through;
//!   2. create a GL surface in the window, below the webview, that mpv renders into.
//!
//! Note what is deliberately absent: window-level (desktop) transparency. Compositing here
//! happens *inside* one window, between two widgets, so the window itself can stay opaque. That
//! avoids needing a compositing WM on Linux and, on macOS, avoids Tauri's `macos-private-api`
//! feature and its App Store implications entirely.

use serde::{Deserialize, Serialize};

/// Where the video should appear, in the window's logical (CSS) pixel coordinates — the same
/// space `getBoundingClientRect()` reports, so the frontend can pass it through unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VideoRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl VideoRect {
    /// Clamp to something a toolkit will accept: non-negative size, integral position.
    ///
    /// A zero-size rect is legitimate (the element is scrolled out of view or `display:none`),
    /// and callers use `is_empty` to hide the surface rather than trying to draw into it.
    pub fn normalized(&self) -> (i32, i32, i32, i32) {
        let w = if self.width.is_finite() {
            self.width.max(0.0).round() as i32
        } else {
            0
        };
        let h = if self.height.is_finite() {
            self.height.max(0.0).round() as i32
        } else {
            0
        };
        let x = if self.x.is_finite() {
            self.x.round() as i32
        } else {
            0
        };
        let y = if self.y.is_finite() {
            self.y.round() as i32
        } else {
            0
        };
        (x, y, w, h)
    }

    pub fn is_empty(&self) -> bool {
        let (_, _, w, h) = self.normalized();
        w <= 0 || h <= 0
    }
}

#[cfg(any(target_os = "linux", windows, target_os = "macos"))]
pub mod gl;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(windows)]
pub mod windows;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_fractional_rects() {
        let r = VideoRect {
            x: 10.4,
            y: 20.6,
            width: 100.5,
            height: 50.4,
        };
        assert_eq!(r.normalized(), (10, 21, 101, 50));
    }

    #[test]
    fn clamps_negative_size_to_zero() {
        let r = VideoRect {
            x: 0.0,
            y: 0.0,
            width: -5.0,
            height: 10.0,
        };
        assert_eq!(r.normalized(), (0, 0, 0, 10));
        assert!(r.is_empty());
    }

    #[test]
    fn treats_non_finite_as_empty() {
        let r = VideoRect {
            x: f64::NAN,
            y: 0.0,
            width: f64::INFINITY,
            height: 10.0,
        };
        assert_eq!(r.normalized(), (0, 0, 0, 10));
        assert!(r.is_empty());
    }

    #[test]
    fn offscreen_element_is_empty_not_an_error() {
        // A collapsed element (display:none) reports a zero rect; that means "hide", not "fail".
        let r = VideoRect {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        };
        assert!(r.is_empty());
    }

    #[test]
    fn negative_position_is_preserved() {
        // Scrolled partly above the viewport: the surface should follow, not clamp to 0.
        let r = VideoRect {
            x: -30.0,
            y: -12.7,
            width: 200.0,
            height: 100.0,
        };
        assert_eq!(r.normalized(), (-30, -13, 200, 100));
        assert!(!r.is_empty());
    }
}
