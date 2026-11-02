//! Pinned Wintun distribution, shared by proxy TUN and mesh workers.
use crate::{Error, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const HASH: &str = "07c256185d6ee3652e09fa55c0b673e2624b565e02c4b9091c79ca7d2f24ef51";
pub fn archive_path(paths: &crate::paths::AppPaths) -> PathBuf {
    paths.data_dir.join("mesh-binaries/wintun-0.14.1.zip")
}

fn verified(path: &Path) -> Result<Vec<u8>> {
    let bytes =
        std::fs::read(path).map_err(|_| Error::binary("缺少 Wintun 驱动，请重新连接以下载组件"))?;
    if hex::encode(Sha256::digest(&bytes)) != HASH {
        return Err(Error::binary("Wintun 驱动摘要不匹配，请重新下载"));
    }
    Ok(bytes)
}

pub async fn install(paths: &crate::paths::AppPaths) -> Result<PathBuf> {
    let target = archive_path(paths);
    if verified(&target).is_ok() {
        return Ok(target);
    }
    let bytes = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()?
        .get("https://www.wintun.net/builds/wintun-0.14.1.zip")
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    if hex::encode(Sha256::digest(&bytes)) != HASH {
        return Err(Error::binary("Wintun 下载未通过 SHA-256 校验"));
    }
    std::fs::create_dir_all(target.parent().unwrap())
        .map_err(|_| Error::binary("无法创建驱动目录"))?;
    crate::util::atomic_write(&target, &bytes)?;
    Ok(target)
}

pub fn unpack(archive: &Path, directory: &Path) -> Result<PathBuf> {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        _ => return Err(Error::binary("组网仅支持 Windows x64/ARM64")),
    };
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(verified(archive)?))
        .map_err(|_| Error::binary("Wintun 压缩包无效"))?;
    let target = directory.join("wintun.dll");
    for (source, path) in [
        (format!("wintun/bin/{arch}/wintun.dll"), target.clone()),
        (
            "wintun/LICENSE.txt".into(),
            directory.join("WINTUN-LICENSE.txt"),
        ),
    ] {
        let mut entry = zip
            .by_name(&source)
            .map_err(|_| Error::binary("Wintun 压缩包内容不完整"))?;
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut bytes)
            .map_err(|_| Error::binary("读取 Wintun 失败"))?;
        // A connected backend may have the same DLL loaded. Never replace an
        // already verified copy, including during apply/restart operations.
        if std::fs::read(&path).ok().as_deref() != Some(bytes.as_slice()) {
            crate::util::atomic_write(&path, &bytes)?;
        }
    }
    Ok(target)
}

#[cfg(windows)]
pub fn load(path: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // Keep the module resident until the worker exits. No GUI-side driver load.
    let module = unsafe {
        windows_sys::Win32::System::LibraryLoader::LoadLibraryExW(
            wide.as_ptr(),
            std::ptr::null_mut(),
            0x1100,
        )
    };
    if module.is_null() {
        return Err(Error::binary("无法加载 Wintun，请检查系统驱动策略"));
    }
    Ok(())
}
