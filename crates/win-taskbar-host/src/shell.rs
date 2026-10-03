//! Taskbar discovery, geometry and layout.

use std::{mem::zeroed, ptr::null};
use windows_sys::Win32::{
    Foundation::{HWND, RECT},
    Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromWindow},
    UI::{HiDpi::*, WindowsAndMessaging::*},
};

use crate::wide;

/// Gap above and below content that fills the taskbar height, in DIPs.
const INSET_DIP: f64 = 4.0;
/// Explorer's border line on the taskbar edge that faces the desktop, inside its window.
const BORDER_DIP: f64 = 1.0;

/// One discovered taskbar window and the Explorer thread that owns it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Taskbar {
    pub hwnd: HWND,
    pub pid: u32,
    pub tid: u32,
}

impl Taskbar {
    pub fn find_primary() -> Option<Self> {
        let hwnd = unsafe { FindWindowW(wide("Shell_TrayWnd").as_ptr(), null()) };
        Self::from_hwnd(hwnd)
    }

    fn from_hwnd(hwnd: HWND) -> Option<Self> {
        if hwnd.is_null() || class_name(hwnd) != "Shell_TrayWnd" {
            return None;
        }
        let mut pid = 0;
        let tid = unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        (tid != 0).then_some(Self { hwnd, pid, tid })
    }

    /// Windows reuses HWND values, so this also compares class, process and thread.
    pub fn is_alive(&self) -> bool {
        Self::from_hwnd(self.hwnd) == Some(*self)
    }

    pub fn geometry(&self) -> Geometry {
        let mut client: RECT = unsafe { zeroed() };
        let mut window: RECT = unsafe { zeroed() };
        let mut monitor: MONITORINFO = unsafe { zeroed() };
        monitor.cbSize = size_of::<MONITORINFO>() as u32;
        unsafe {
            GetClientRect(self.hwnd, &mut client);
            GetWindowRect(self.hwnd, &mut window);
            let handle = MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTOPRIMARY);
            GetMonitorInfoW(handle, &mut monitor);
        }
        let screen = monitor.rcMonitor;
        Geometry {
            width: client.right,
            height: client.bottom,
            dpi: unsafe { GetDpiForWindow(self.hwnd) }.max(96),
            // Auto-hide slides the window partly off-screen. Its center still tells the edge.
            top: window.top + window.bottom < screen.top + screen.bottom,
        }
    }
}

/// Taskbar client size in physical pixels, its DPI and where it sits.
pub(crate) struct Geometry {
    pub width: i32,
    pub height: i32,
    pub dpi: u32,
    /// Meaningful for horizontal taskbars only.
    pub top: bool,
}

/// Where the content goes inside a horizontal taskbar's client area, in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Layout {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// `x` ranges over `0..=travel`.
    pub travel: i32,
    pub dpi: u32,
}

impl Layout {
    /// Places content of the requested DIP size at a normalized position along
    /// the taskbar, centered vertically. `None` if it does not fit, or the
    /// taskbar is vertical.
    pub fn compute(
        geometry: &Geometry,
        width_dip: f64,
        height_dip: Option<f64>,
        position: f64,
    ) -> Option<Self> {
        let scale = geometry.dpi as f64 / 96.0;
        let width = (width_dip * scale).round() as i32;
        let height = match height_dip {
            Some(h) => (h * scale).round() as i32,
            None => geometry.height - 2 * (INSET_DIP * scale).round() as i32,
        };
        if geometry.width < geometry.height
            || width <= 0
            || height <= 0
            || width > geometry.width
            || height > geometry.height
        {
            return None;
        }
        let travel = geometry.width - width;
        // Center in the area beside the border, like the taskbar's icons, with any odd pixel above.
        let border = (BORDER_DIP * scale).round() as i32 * if geometry.top { -1 } else { 1 };
        Some(Self {
            x: (position.clamp(0.0, 1.0) * travel as f64).round() as i32,
            y: ((geometry.height - height + border + 1) / 2).clamp(0, geometry.height - height),
            width,
            height,
            travel,
            dpi: geometry.dpi,
        })
    }

    /// Normalized position for a horizontal offset in pixels.
    pub fn position_of(&self, x: i32) -> f64 {
        x.clamp(0, self.travel) as f64 / self.travel.max(1) as f64
    }
}

fn class_name(hwnd: HWND) -> String {
    let mut buffer = [0u16; 64];
    let len = unsafe { GetClassNameW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
    String::from_utf16_lossy(&buffer[..len.max(0) as usize])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(width: i32, height: i32, dpi: u32) -> Geometry {
        Geometry { width, height, dpi, top: false }
    }

    #[test]
    fn scales_centers_and_positions_along_the_taskbar() {
        let layout = Layout::compute(&bar(3840, 72, 144), 160.0, None, 0.25).unwrap();
        assert_eq!((layout.width, layout.height), (240, 60));
        assert_eq!(layout.y, 7);
        assert_eq!(layout.travel, 3600);
        assert_eq!(layout.x, 900);
        assert_eq!(layout.position_of(layout.x), 0.25);
    }

    #[test]
    fn clamps_position_and_offsets_to_the_taskbar() {
        let layout = |p| Layout::compute(&bar(1920, 48, 96), 160.0, Some(32.0), p).unwrap();
        assert_eq!((layout(7.0).x, layout(-1.0).x), (1760, 0));
        let layout = layout(0.5);
        assert_eq!((layout.position_of(-50), layout.position_of(99_999)), (0.0, 1.0));
    }

    #[test]
    fn reports_content_that_cannot_fit() {
        assert!(Layout::compute(&bar(1920, 48, 96), 2000.0, None, 0.0).is_none());
        assert!(Layout::compute(&bar(1920, 48, 96), 100.0, Some(60.0), 0.0).is_none());
        assert!(Layout::compute(&bar(48, 1920, 96), 40.0, None, 0.0).is_none());
    }
}
