//! One-time installation of offline resources from a full desktop distribution.

use std::{collections::BTreeMap, fs, io, path::Path};

use crate::{
    Error, Result, Settings,
    core::binary::BinaryManager,
    mesh::{TAILSCALE_CORE_VERSION, tailscale},
    model::CoreKind,
    paths::AppPaths,
    util::{atomic_write, set_executable, sha256_file},
};
use fs2::FileExt;
use serde::Deserialize;

#[derive(Deserialize)]
struct Resources {
    rules: BTreeMap<String, Rule>,
}

#[derive(Deserialize)]
struct Rule {
    sha256: String,
}

fn copy_missing(source: &Path, dest: &Path, executable: bool) -> Result<()> {
    if dest.is_file() {
        return Ok(());
    }
    let parent = dest.parent().unwrap();
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    io::copy(&mut fs::File::open(source)?, &mut temp)?;
    if executable {
        set_executable(temp.path())?;
    }
    temp.as_file().sync_all()?;
    match temp.persist_noclobber(dest) {
        Ok(_) => Ok(()),
        Err(e) if e.error.kind() == io::ErrorKind::AlreadyExists && dest.is_file() => Ok(()),
        Err(e) => Err(e.error.into()),
    }
}

/// Run before the first settings file is created. An interrupted installation is retryable.
pub fn prepare(paths: &AppPaths) -> Result<()> {
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .ok_or_else(|| Error::config("无法确定应用安装目录"))?;
    #[cfg(target_os = "macos")]
    let root = dir.join("../Resources/xrose-seed");
    #[cfg(windows)]
    let root = {
        // A portable lite update can be unzipped over an older full directory.
        if fs::read_to_string(dir.join("xrose.portable")).is_ok_and(|v| v.trim() == "lite") {
            return Ok(());
        }
        dir.join("resources/xrose-seed")
    };
    #[cfg(not(any(windows, target_os = "macos")))]
    let root = dir.join("../share/xrose/xrose-seed");
    if !root.try_exists()? {
        return Ok(());
    }
    install(paths, &root)
}

fn install(paths: &AppPaths, root: &Path) -> Result<()> {
    let done = paths.data_dir.join("full-seed-v1");
    if done.is_file() {
        return Ok(());
    }
    paths.ensure()?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(paths.data_dir.join("full-seed.lock"))?;
    FileExt::lock_exclusive(&lock)?;
    if done.is_file() {
        return Ok(());
    }

    let (asset, _) = tailscale::release()?;
    let archive = root.join(asset);
    tailscale::verify_binary(&archive)?;
    let resources: Resources =
        serde_json::from_str(include_str!("../../../packaging/resources.lock.json"))?;
    for (name, rule) in resources.rules {
        let source = root.join(&name);
        if sha256_file(&source)? != rule.sha256 {
            return Err(Error::binary(format!(
                "随附规则集 {name} 校验失败，请重新获取完整包"
            )));
        }
        copy_missing(&source, &paths.rule_sets_dir().join(name), false)?;
    }
    copy_missing(&archive, &tailscale::archive_path(paths), false)?;
    let cli = format!("xrose-cli{}", std::env::consts::EXE_SUFFIX);
    copy_missing(
        &root.join(&cli),
        &paths.data_dir.join("bin").join(cli),
        true,
    )?;

    let manager = BinaryManager::new(paths);
    let core = CoreKind::SingBox;
    let version = TAILSCALE_CORE_VERSION;
    if manager.resolve(core, Some(version)).is_none() {
        let dir = manager.root().join(core.as_str()).join(version);
        fs::create_dir_all(&dir)?;
        let tmp = tempfile::tempdir_in(&dir)?;
        let source = tailscale::unpack(&archive, tmp.path())?;
        let dest = dir.join(source.file_name().unwrap());
        copy_missing(&source, &dest, true)?;
        if sha256_file(&dest)? != sha256_file(&source)? {
            return Err(Error::binary(
                "已有同版本 sing-box 与随附内核不符，请检查内核目录",
            ));
        }
        manager.adopt_existing(core, version, &dest)?;
    }
    if !paths.settings_file().exists() {
        let mut settings = Settings::default();
        settings
            .active_versions
            .insert(core.as_str().into(), version.into());
        settings.save(&paths.settings_file())?;
    }
    atomic_write(&done, b"1\n")
}
