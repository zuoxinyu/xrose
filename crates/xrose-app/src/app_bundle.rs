//! Supply a real application identity when launched by Cargo or as a standalone binary.
//! The helper entry point bypasses this module. Packaged applications are never modified.

use std::fs::{self, OpenOptions};
use std::os::unix::{fs::MetadataExt, process::CommandExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use cocoa::base::{id, nil};
use cocoa::foundation::{NSAutoreleasePool, NSString};
use fs2::FileExt;
use objc::{class, msg_send, sel, sel_impl};

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn LSRegisterURL(url: *const std::ffi::c_void, update: u8) -> i32;
}

/// Re-exec preserves Cargo's terminal, process ID, arguments, environment and exit status.
pub fn ensure(fallback_directory: &Path) -> anyhow::Result<()> {
    let executable = std::env::current_exe()?;
    if let Some(bundle) = bundle_root(&executable) {
        return register(&bundle);
    }
    // Keep application identity independent of profiles. Launch Services does not
    // accept applications in /tmp, a common XROSE_HOME location for development.
    let adjacent = executable.parent().unwrap().join(".xrose-desktop");
    let directory = if fs::create_dir_all(&adjacent).is_ok() {
        adjacent
    } else {
        fs::create_dir_all(fallback_directory)?;
        fallback_directory.to_path_buf()
    };
    // Canonicalize before exec so relative --home/XROSE_HOME paths remain valid.
    let directory = directory.canonicalize()?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join("bundle.lock"))?;
    lock.lock_exclusive()?;
    let bundle = prepare_bundle(&directory, &executable)?;
    // Never carry the preparation lock across exec (or hold it for the GUI lifetime).
    drop(lock);
    Err(Command::new(bundle.join("Contents/MacOS/xrose-app"))
        .args(std::env::args_os().skip(1))
        .exec()
        .into())
}

fn bundle_root(executable: &Path) -> Option<PathBuf> {
    let macos = executable.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension()? == "app"
        && contents.join("Info.plist").is_file())
    .then(|| bundle.to_path_buf())
}

fn prepare_bundle(directory: &Path, executable: &Path) -> anyhow::Result<PathBuf> {
    let metadata = fs::metadata(executable)?;
    let stamp = format!(
        "1\n{}\n{}:{}:{}:{}:{}\n{}",
        executable.display(),
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        env!("CARGO_PKG_VERSION")
    );
    let bundle = directory.join("Xrose.app");
    if fs::read_to_string(bundle.join("Contents/Resources/xrose-build"))
        .ok()
        .as_deref()
        == Some(&stamp)
        && bundle.join("Contents/MacOS/xrose-app").is_file()
    {
        return Ok(bundle);
    }
    // Build and sign in a sibling staging directory before replacing our generated bundle.
    let staging = tempfile::tempdir_in(directory)?;
    let prepared = staging.path().join("Xrose.app");
    write_bundle(&prepared, executable, &stamp)?;
    let signed = Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "-", "--identifier", "io.xrose.Xrose"])
        .arg(&prepared)
        .output()?;
    anyhow::ensure!(
        signed.status.success(),
        "应用签名失败：{}",
        String::from_utf8_lossy(&signed.stderr).trim()
    );
    let previous = staging.path().join("previous.app");
    let had_previous = bundle.exists();
    if had_previous {
        fs::rename(&bundle, &previous)?;
    }
    if let Err(error) = fs::rename(&prepared, &bundle) {
        if had_previous {
            fs::rename(&previous, &bundle)?;
        }
        return Err(error.into());
    }
    Ok(bundle)
}

fn write_bundle(bundle: &Path, executable: &Path, stamp: &str) -> std::io::Result<()> {
    let contents = bundle.join("Contents");
    fs::create_dir_all(contents.join("MacOS"))?;
    fs::create_dir_all(contents.join("Resources"))?;
    // Copy rather than symlink: NSBundle resolves executable symlinks outside the bundle.
    fs::copy(executable, contents.join("MacOS/xrose-app"))?;
    fs::write(
        contents.join("Info.plist"),
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>io.xrose.Xrose</string>
<key>CFBundleName</key><string>Xrose</string>
<key>CFBundleDisplayName</key><string>Xrose</string>
<key>CFBundleExecutable</key><string>xrose-app</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>{0}</string>
<key>CFBundleVersion</key><string>{0}</string>
<key>CFBundleIconFile</key><string>Xrose.icns</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
"#,
            env!("CARGO_PKG_VERSION")
        ),
    )?;
    // ic08 stores a 256px PNG without requiring iconutil or development tools at runtime.
    let png = include_bytes!("../assets/app-icon-256.png");
    let mut icon = Vec::from(*b"icns");
    icon.extend_from_slice(&((png.len() + 16) as u32).to_be_bytes());
    icon.extend_from_slice(b"ic08");
    icon.extend_from_slice(&((png.len() + 8) as u32).to_be_bytes());
    icon.extend_from_slice(png);
    fs::write(contents.join("Resources/Xrose.icns"), icon)?;
    fs::write(contents.join("Resources/xrose-build"), stamp)
}

fn register(bundle: &Path) -> anyhow::Result<()> {
    unsafe {
        let pool = NSAutoreleasePool::new(nil);
        let path = NSString::alloc(nil).init_str(&bundle.to_string_lossy());
        let url: id =
            msg_send![class!(NSURL), fileURLWithPath: path isDirectory: objc::runtime::YES];
        let status = LSRegisterURL(url.cast(), 1);
        let _: () = msg_send![path, release];
        let _: () = msg_send![pool, drain];
        anyhow::ensure!(status == 0, "注册应用身份失败（{status}）");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_contains_real_executable_and_stable_identity() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        fs::write(&source, b"fixture binary").unwrap();
        let bundle = directory.path().join("中文 folder/Xrose.app");
        write_bundle(&bundle, &source, "fixture").unwrap();
        let executable = bundle.join("Contents/MacOS/xrose-app");
        assert_eq!(bundle_root(&executable), Some(bundle.clone()));
        assert!(bundle_root(&source).is_none());
        assert!(
            !executable
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(executable).unwrap(), b"fixture binary");
        let plist = fs::read_to_string(bundle.join("Contents/Info.plist")).unwrap();
        assert!(plist.contains("<string>io.xrose.Xrose</string>"));
        assert!(plist.contains("<string>xrose-app</string>"));
        let icon = fs::read(bundle.join("Contents/Resources/Xrose.icns")).unwrap();
        assert_eq!(&icon[..4], b"icns");
        assert_eq!(
            u32::from_be_bytes(icon[4..8].try_into().unwrap()) as usize,
            icon.len()
        );
    }

    #[test]
    fn generated_bundle_is_signed_and_reused_when_source_is_unchanged() {
        let directory = tempfile::tempdir().unwrap();
        let source = std::env::current_exe().unwrap();
        let bundle = prepare_bundle(directory.path(), &source).unwrap();
        let result = Command::new("/usr/bin/codesign")
            .args(["--verify", "--strict"])
            .arg(&bundle)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let executable = bundle.join("Contents/MacOS/xrose-app");
        let inode = fs::metadata(&executable).unwrap().ino();
        assert_eq!(prepare_bundle(directory.path(), &source).unwrap(), bundle);
        assert_eq!(fs::metadata(executable).unwrap().ino(), inode);
    }
}
