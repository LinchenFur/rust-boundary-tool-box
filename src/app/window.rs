//! 主窗口尺寸自适应。

use crate::AppWindow;

use slint::{ComponentHandle, LogicalSize, PhysicalPosition};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::{GetDC, GetDeviceCaps, LOGPIXELSX, ReleaseDC};
use windows::Win32::UI::WindowsAndMessaging::{
    SPI_GETWORKAREA, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
};

const DEFAULT_WIDTH: f32 = 1260.0;
const DEFAULT_HEIGHT: f32 = 780.0;
const MIN_COMFORT_WIDTH: f32 = 980.0;
const MIN_COMFORT_HEIGHT: f32 = 600.0;
const WIDTH_FILL: f32 = 0.92;
const HEIGHT_FILL: f32 = 0.90;
const EMERGENCY_FILL: f32 = 0.96;

#[derive(Clone, Copy, Debug, PartialEq)]
struct WorkArea {
    left: i32,
    top: i32,
    width: u32,
    height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct AdaptiveWindowGeometry {
    logical_size: LogicalSize,
    physical_position: PhysicalPosition,
}

/// 根据当前 Windows 工作区调整初始窗口尺寸，避免在小屏或高 DPI 缩放下超出屏幕。
pub(super) fn apply_adaptive_window_geometry(app: &AppWindow) {
    let Some(work_area) = primary_work_area() else {
        return;
    };
    let geometry = calculate_adaptive_window_geometry(work_area, system_dpi_scale());
    app.window().set_size(geometry.logical_size);
    app.window().set_position(geometry.physical_position);
}

fn calculate_adaptive_window_geometry(
    work_area: WorkArea,
    dpi_scale: f32,
) -> AdaptiveWindowGeometry {
    let dpi_scale = dpi_scale.max(1.0);
    let available_width = work_area.width as f32 / dpi_scale;
    let available_height = work_area.height as f32 / dpi_scale;
    let logical_width = adaptive_length(
        available_width,
        DEFAULT_WIDTH,
        MIN_COMFORT_WIDTH,
        WIDTH_FILL,
    );
    let logical_height = adaptive_length(
        available_height,
        DEFAULT_HEIGHT,
        MIN_COMFORT_HEIGHT,
        HEIGHT_FILL,
    );
    let physical_width = (logical_width * dpi_scale).round() as i32;
    let physical_height = (logical_height * dpi_scale).round() as i32;
    let physical_position = PhysicalPosition::new(
        work_area.left + ((work_area.width as i32 - physical_width).max(0) / 2),
        work_area.top + ((work_area.height as i32 - physical_height).max(0) / 2),
    );

    AdaptiveWindowGeometry {
        logical_size: LogicalSize::new(logical_width.round(), logical_height.round()),
        physical_position,
    }
}

fn adaptive_length(available: f32, preferred: f32, comfort_minimum: f32, fill: f32) -> f32 {
    if available <= 0.0 {
        return preferred;
    }

    let target = preferred.min(available * fill);
    let emergency_minimum = comfort_minimum.min(available * EMERGENCY_FILL);
    target.max(emergency_minimum).min(available)
}

fn primary_work_area() -> Option<WorkArea> {
    let mut rect = RECT::default();
    unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some((&mut rect as *mut RECT).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .ok()?;
    }

    let width = (rect.right - rect.left).max(0) as u32;
    let height = (rect.bottom - rect.top).max(0) as u32;
    if width == 0 || height == 0 {
        None
    } else {
        Some(WorkArea {
            left: rect.left,
            top: rect.top,
            width,
            height,
        })
    }
}

fn system_dpi_scale() -> f32 {
    unsafe {
        let hdc = GetDC(None);
        if hdc.is_invalid() {
            return 1.0;
        }
        let dpi = GetDeviceCaps(Some(hdc), LOGPIXELSX);
        let _ = ReleaseDC(None, hdc);
        if dpi > 0 { dpi as f32 / 96.0 } else { 1.0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_default_size_when_work_area_is_large() {
        let geometry = calculate_adaptive_window_geometry(
            WorkArea {
                left: 0,
                top: 0,
                width: 1920,
                height: 1040,
            },
            1.0,
        );

        assert_eq!(geometry.logical_size, LogicalSize::new(1260.0, 780.0));
        assert_eq!(geometry.physical_position, PhysicalPosition::new(330, 130));
    }

    #[test]
    fn shrinks_height_for_768p_work_area() {
        let geometry = calculate_adaptive_window_geometry(
            WorkArea {
                left: 0,
                top: 0,
                width: 1366,
                height: 728,
            },
            1.0,
        );

        assert_eq!(geometry.logical_size.width, 1257.0);
        assert_eq!(geometry.logical_size.height, 655.0);
        assert_eq!(geometry.physical_position.y, 36);
    }

    #[test]
    fn accounts_for_high_dpi_scaling() {
        let geometry = calculate_adaptive_window_geometry(
            WorkArea {
                left: 0,
                top: 0,
                width: 1920,
                height: 1040,
            },
            1.5,
        );

        assert_eq!(geometry.logical_size.width, 1178.0);
        assert_eq!(geometry.logical_size.height, 624.0);
        assert_eq!(geometry.physical_position.y, 52);
    }
}
