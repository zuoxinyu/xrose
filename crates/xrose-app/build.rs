//! Windows gives the process main thread a 1 MiB stack by default. GPUI
//! renders the page as one deeply nested `Div` tree, and in debug builds each
//! layout frame is large enough that the settings page (the deepest page, and
//! deeper still once a core version row is rendered) exhausts that stack and
//! aborts with a stack overflow (`0xC00000FD`).
//!
//! Raise the executable's reserved main-thread stack so rendering has headroom.
//! Only the `xrose-app` binaries are affected; the CLI and library keep the
//! platform default.
//!
//! On Windows the app icon is embedded as resource ID 1. GPUI's Windows
//! platform loads that resource for the window class (`LoadImageW(... ICON, 1)`)
//! and the shell uses it for the taskbar and Explorer, so the PNG cannot just
//! stay in `assets/`. The `.ico` is generated here from the 256 px PNG using
//! the ICO-with-PNG-entry form supported since Vista.

use std::fs;
use std::path::PathBuf;

const STACK_BYTES: u64 = 16 * 1024 * 1024;
const ICON_PNG: &str = "assets/app-icon-256.png";

#[path = "../../build_support/easytier_native.rs"]
mod easytier_native;

fn main() {
    easytier_native::configure();
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={ICON_PNG}");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos")
        && std::env::var("PROFILE").as_deref() == Ok("debug")
    {
        // GPUI plus static EasyTier exceeds Mach-O's 24-bit compact unwind
        // offsets in debug builds. Keep full DWARF unwinding in that profile.
        println!("cargo:rustc-link-arg-bins=-Wl,-no_compact_unwind");
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if env == "msvc" {
        println!("cargo:rustc-link-arg-bins=/STACK:{STACK_BYTES}");
    } else {
        println!("cargo:rustc-link-arg-bins=-Wl,--stack,{STACK_BYTES}");
    }
    if let Err(err) = embed_windows_icon() {
        // A missing resource compiler should not make the app unbuildable:
        // the window still works, only the taskbar icon stays generic.
        println!("cargo:warning=failed to embed Windows app icon: {err}");
    }
}

fn embed_windows_icon() -> Result<(), Box<dyn std::error::Error>> {
    let png = fs::read(ICON_PNG)?;
    let out_dir = PathBuf::from(std::env::var("OUT_DIR")?);
    let ico_path = out_dir.join("xrose-app.ico");

    // ICONDIR + one 256x256 PNG entry (width/height 0 mean 256).
    let mut ico = Vec::with_capacity(png.len() + 22);
    ico.extend_from_slice(&0u16.to_le_bytes()); // reserved
    ico.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    ico.extend_from_slice(&1u16.to_le_bytes()); // image count
    ico.extend_from_slice(&[0, 0, 0, 0]); // width, height, colors, reserved
    ico.extend_from_slice(&1u16.to_le_bytes()); // color planes
    ico.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
    ico.extend_from_slice(&(png.len() as u32).to_le_bytes());
    ico.extend_from_slice(&22u32.to_le_bytes()); // image offset
    ico.extend_from_slice(&png);
    fs::write(&ico_path, ico)?;

    let rc_path = out_dir.join("xrose-app.rc");
    let icon_literal = ico_path.display().to_string().replace('\\', "\\\\");
    fs::write(&rc_path, format!("1 ICON \"{icon_literal}\"\n"))?;
    let _ = embed_resource::compile(&rc_path, embed_resource::NONE);
    Ok(())
}
