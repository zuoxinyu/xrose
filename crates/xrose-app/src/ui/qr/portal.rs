//! Screenshot portal capture for the KDE Wayland viewfinder.
use super::platform::CaptureError;
use ashpd::{
    Error, PortalError,
    desktop::{ResponseError, screenshot::Screenshot},
};
use image::{ImageReader, RgbaImage};
use std::{fs::File, io::BufReader};

const UNAVAILABLE: CaptureError = (
    "桌面截图服务未能完成截图，请检查 KDE 截图服务后重试",
    "The desktop screenshot service failed. Check the KDE screenshot portal and try again.",
);
const READ_FAILED: CaptureError = (
    "无法读取系统返回的截图，请重试",
    "Could not read the screenshot returned by the desktop. Try again.",
);

fn capture_error(error: Error) -> CaptureError {
    match error {
        Error::Portal(PortalError::NotAllowed(_)) => (
            "截图请求被拒绝，请在系统设置的应用程序权限中允许 Xrose 截图",
            "Screenshot access was denied. Allow screenshots for Xrose in System Settings.",
        ),
        Error::Response(ResponseError::Cancelled) | Error::Portal(PortalError::Cancelled(_)) => (
            "桌面截图服务已取消请求。若未手动取消，请检查当前桌面的截图服务",
            "The desktop portal cancelled the screenshot. If you did not cancel, check the screenshot portal for this desktop.",
        ),
        _ => UNAVAILABLE,
    }
}

pub(super) async fn capture(screen: [f32; 4], region: [f32; 4]) -> Result<RgbaImage, CaptureError> {
    let connection = ashpd::zbus::Connection::session()
        .await
        .map_err(|_| UNAVAILABLE)?;
    // The permission store keys host apps by desktop ID on this D-Bus connection.
    // Older portals and connections already identified by the launcher can still capture.
    let _ = ashpd::register_host_app_with_connection(
        connection.clone(),
        crate::APP_ID.parse().unwrap(),
    )
    .await;
    let shot = Screenshot::request()
        .connection(Some(connection))
        .interactive(false)
        .send()
        .await
        .and_then(|request| request.response())
        .map_err(capture_error)?;
    let path = url::Url::parse(shot.uri().as_str())
        .ok()
        .and_then(|u| u.to_file_path().ok())
        .ok_or(READ_FAILED)?;
    let file = File::open(&path);
    // Unlink the portal's transient screenshot before decoding, including on a decode failure.
    let _ = std::fs::remove_file(path);
    let reader = ImageReader::new(BufReader::new(file.map_err(|_| READ_FAILED)?))
        .with_guessed_format()
        .map_err(|_| READ_FAILED)?;
    let mut desktop = reader.decode().map_err(|_| READ_FAILED)?;
    let (x, y, w, h) =
        crop_bounds((desktop.width(), desktop.height()), screen, region).ok_or(READ_FAILED)?;
    Ok(desktop.crop(x, y, w, h).into_rgba8())
}

fn crop_bounds(
    (width, height): (u32, u32),
    screen: [f32; 4],
    region: [f32; 4],
) -> Option<(u32, u32, u32, u32)> {
    let axis = |pixels: u32, i: usize| {
        let start = screen[i] + region[i] * screen[i + 2];
        let end = start + region[i + 2] * screen[i + 2];
        let first = (start * pixels as f32).floor() as u32;
        let last = ((end * pixels as f32).ceil() as u32).min(pixels);
        (first, last.saturating_sub(first))
    };
    let (x, w) = axis(width, 0);
    let (y, h) = axis(height, 1);
    (w > 0 && h > 0).then_some((x, y, w, h))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn region_is_relative_to_the_selected_output_in_the_returned_image() {
        assert_eq!(
            crop_bounds((5120, 1600), [0.5, 0., 0.5, 1.], [0.25, 0.25, 0.5, 0.5]),
            Some((3200, 400, 1280, 800))
        );
        assert_eq!(
            crop_bounds((1463, 914), [0., 0., 1., 1.], [0., 0., 1., 1.]),
            Some((0, 0, 1463, 914))
        );
    }
}
