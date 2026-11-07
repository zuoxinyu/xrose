//! Work around upstream's workspace-relative Packet.lib search path.
pub fn configure() {
    println!("cargo:rerun-if-env-changed=XROSE_EASYTIER_SOURCE");
    println!("cargo:rerun-if-env-changed=CARGO_HOME");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").unwrap().as_str() {
        "x86_64" => "x86_64",
        "aarch64" => "arm64",
        other => panic!("Unsupported EasyTier architecture: {other}"),
    };
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(
                std::env::var_os("USERPROFILE").expect("CARGO_HOME or USERPROFILE"),
            )
            .join(".cargo")
        });
    let mut candidates = Vec::new();
    if let Some(root) = std::env::var_os("XROSE_EASYTIER_SOURCE") {
        candidates.push(std::path::PathBuf::from(root));
    }
    if let Ok(repos) = std::fs::read_dir(cargo_home.join("git/checkouts")) {
        for repo in repos
            .flatten()
            .filter(|r| r.file_name().to_string_lossy().starts_with("easytier-"))
        {
            if let Ok(revisions) = std::fs::read_dir(repo.path()) {
                candidates.extend(
                    revisions
                        .flatten()
                        .filter(|r| r.file_name().to_string_lossy().starts_with("8428a89"))
                        .map(|r| r.path()),
                );
            }
        }
    }
    let native=candidates.into_iter().map(|r|r.join("easytier/third_party").join(arch)).find(|p|p.join("Packet.lib").is_file()).expect("Pinned EasyTier native libraries not found. Run cargo fetch --locked, or set XROSE_EASYTIER_SOURCE to the v2.6.4 source root.");
    println!("cargo:rustc-link-search=native={}", native.display());
    // Windows' normal adapter enumeration does not use Packet.dll. Delay loading
    // keeps the optional pnet fallback out of GUI startup; no Npcap DLL is shipped.
    println!("cargo:rustc-link-lib=delayimp");
    println!("cargo:rustc-link-arg=/DELAYLOAD:Packet.dll");
}
