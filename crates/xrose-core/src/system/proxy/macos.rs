//! Manage the proxy settings of enabled macOS network services.

use super::*;

#[derive(Clone, Copy)]
struct Kind {
    key: &'static str,
    get: &'static str,
    set: &'static str,
    state: &'static str,
}

const KINDS: [Kind; 4] = [
    Kind {
        key: "webproxy",
        get: "-getwebproxy",
        set: "-setwebproxy",
        state: "-setwebproxystate",
    },
    Kind {
        key: "securewebproxy",
        get: "-getsecurewebproxy",
        set: "-setsecurewebproxy",
        state: "-setsecurewebproxystate",
    },
    Kind {
        key: "socksproxy",
        get: "-getsocksfirewallproxy",
        set: "-setsocksfirewallproxy",
        state: "-setsocksfirewallproxystate",
    },
    Kind {
        key: "autoproxy",
        get: "-getautoproxyurl",
        set: "-setautoproxyurl",
        state: "-setautoproxystate",
    },
];

fn networksetup(args: &[&str]) -> Result<String> {
    let output = Command::new("/usr/sbin/networksetup")
        .env("LC_ALL", "C")
        .args(args)
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() || stdout.lines().any(|line| line.starts_with("** Error:")) {
        // Arguments and output can contain the original PAC URL.
        return Err(Error::other(format!(
            "macOS 系统代理操作 {} 失败，请检查网络服务与权限后重试",
            args[0]
        )));
    }
    Ok(stdout.trim().to_owned())
}

pub(super) fn capture() -> Result<BTreeMap<String, String>> {
    let services = networksetup(&["-listallnetworkservices"])?;
    let mut entries = BTreeMap::new();
    for service in services
        .lines()
        .skip(1)
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with('*'))
    {
        for k in KINDS {
            let raw = networksetup(&[k.get, service])?;
            k.parse(&raw)?;
            entries.insert(format!("{service}/{}", k.key), raw);
        }
    }
    if entries.is_empty() {
        return Err(Error::other("未找到已启用的网络服务"));
    }
    Ok(entries)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Value {
    pub(super) enabled: bool,
    pub(super) address: Vec<String>,
}

impl Kind {
    fn parse(self, raw: &str) -> Result<Value> {
        let field = |key: &str| {
            raw.lines()
                .filter_map(|l| l.split_once(':'))
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.trim())
        };
        let enabled = match field("Enabled") {
            Some("Yes") => true,
            Some("No") => false,
            _ => return Err(Error::other("无法读取 macOS 代理开关状态")),
        };
        let address = if self.key == "autoproxy" {
            field("URL")
                .filter(|u| !matches!(*u, "" | "(null)"))
                .into_iter()
                .map(str::to_string)
                .collect()
        } else {
            match (field("Server"), field("Port")) {
                (Some(h), Some(p))
                    if !h.is_empty() && h != "(null)" && p.parse::<u16>().is_ok_and(|p| p > 0) =>
                {
                    vec![h.to_string(), p.to_string()]
                }
                _ => vec![],
            }
        };
        if enabled && address.is_empty() {
            return Err(Error::other(
                "macOS 已启用的代理缺少有效地址，请检查网络设置",
            ));
        }
        Ok(Value { enabled, address })
    }

    fn matches(self, current: &Value, target: &Value) -> bool {
        current.enabled == target.enabled
            && (target.address.is_empty() || target.address == current.address)
    }

    fn apply_value(self, service: &str, current: &Value, target: &Value) -> Result<()> {
        if self.matches(current, target) {
            return Ok(());
        }
        // A user may edit the service while other services are being processed.
        let latest = self.parse(&networksetup(&[self.get, service])?)?;
        if &latest != current {
            return Ok(());
        }
        self.write(service, current, target, |args| {
            networksetup(args).map(drop)
        })?;
        let raw = networksetup(&[self.get, service])?;
        if !self.matches(&self.parse(&raw)?, target) {
            return Err(Error::other("macOS 未应用代理设置，请检查网络服务后重试"));
        }
        Ok(())
    }

    fn write(
        self,
        service: &str,
        current: &Value,
        target: &Value,
        mut run: impl FnMut(&[&str]) -> Result<()>,
    ) -> Result<()> {
        let mut enabled = current.enabled;
        if !target.address.is_empty() && target.address != current.address {
            let mut args = vec![self.set, service];
            args.extend(target.address.iter().map(String::as_str));
            run(&args)?;
            // Every networksetup address setter also enables its proxy.
            enabled = true;
        }
        if enabled != target.enabled {
            run(&[
                self.state,
                service,
                if target.enabled { "on" } else { "off" },
            ])?;
        }
        Ok(())
    }
}

fn entry(key: &str) -> Result<(&str, Kind)> {
    key.rsplit_once('/')
        .and_then(|(s, k)| {
            KINDS
                .iter()
                .find(|kind| kind.key == k)
                .map(|kind| (s, *kind))
        })
        .ok_or_else(|| Error::other("系统代理快照无法识别，请检查网络设置"))
}

pub(super) fn targets(
    host: &str,
    port: u16,
    socks: Option<(&str, u16)>,
    pac: Option<&str>,
) -> BTreeMap<String, Value> {
    KINDS
        .into_iter()
        .map(|kind| {
            let address = match (kind.key, pac) {
                ("autoproxy", Some(url)) => vec![url.to_owned()],
                ("autoproxy", None) | (_, Some(_)) => vec![],
                ("socksproxy", None) => socks
                    .map(|(h, p)| vec![h.to_owned(), p.to_string()])
                    .unwrap_or_default(),
                _ => vec![host.to_owned(), port.to_string()],
            };
            (
                kind.key.to_owned(),
                Value {
                    enabled: !address.is_empty(),
                    address,
                },
            )
        })
        .collect()
}

impl Value {
    // An empty target address only changes the switch. networksetup keeps the saved address.
    fn retaining_address(mut self, current: &Self) -> Self {
        if self.address.is_empty() {
            self.address.clone_from(&current.address);
        }
        self
    }

    fn transition_to(&self, target: &Self) -> Vec<Self> {
        let mut states = vec![self.clone()];
        if !target.enabled && self.address != target.address {
            states.push(Self {
                enabled: true,
                address: target.address.clone(),
            });
        }
        if target != self {
            states.push(target.clone());
        }
        states
    }
}

fn save(snapshot: &ProxySnapshot, path: &Path) -> Result<()> {
    crate::util::atomic_write_private(path, &serde_json::to_vec(snapshot)?)
}

// Older recovery files only stored the HTTP endpoint and optional PAC URL.
fn can_restore(
    snapshot: &ProxySnapshot,
    key: &str,
    kind: Kind,
    current: &Value,
    original: &Value,
) -> bool {
    if let Some(writes) = snapshot.macos_writes.get(key) {
        return writes.contains(current);
    }
    let host = snapshot.http_host.as_str();
    let port = snapshot.http_port;
    let legacy = targets(host, port, Some((host, port)), snapshot.pac_url.as_deref());
    current == &legacy[kind.key].clone().retaining_address(original)
}

fn prepare_apply(
    snapshot: &mut ProxySnapshot,
    key: &str,
    raw: &str,
    current: &Value,
    target: &Value,
    refresh: bool,
) -> bool {
    if snapshot
        .macos_writes
        .get(key)
        .is_some_and(|writes| !writes.contains(current))
    {
        if refresh {
            return false;
        }
        // Explicitly taking control again establishes a new restore point.
        snapshot.entries.insert(key.to_owned(), raw.to_owned());
    }
    snapshot
        .macos_writes
        .insert(key.to_owned(), current.transition_to(target));
    true
}

pub(super) fn apply(
    entries: &BTreeMap<String, String>,
    snapshot: &mut ProxySnapshot,
    targets: &BTreeMap<String, Value>,
    refresh: bool,
    path: &Path,
) -> Result<()> {
    let mut plan = Vec::new();
    for (key, raw) in entries {
        let (service, kind) = entry(key)?;
        let current = kind.parse(raw)?;
        let target = targets[kind.key].clone().retaining_address(&current);
        if prepare_apply(snapshot, key, raw, &current, &target, refresh) {
            plan.push((key, service, kind, current, target));
        }
    }
    // Journal all possible intermediate values before changing the first service.
    save(snapshot, path)?;
    for (key, service, kind, current, target) in plan {
        kind.apply_value(service, &current, &target)?;
        snapshot.macos_writes.insert(key.clone(), vec![target]);
    }
    save(snapshot, path)
}

pub(super) fn restore(mut snapshot: ProxySnapshot, path: &Path) -> Result<()> {
    let mut error = None;
    let mut plan = Vec::new();
    for (key, raw) in &snapshot.entries {
        let change = (|| -> Result<_> {
            let (service, kind) = entry(key)?;
            let current = kind.parse(&networksetup(&[kind.get, service])?)?;
            let original = kind.parse(raw)?;
            if !can_restore(&snapshot, key, kind, &current, &original) {
                return Ok(None);
            }
            let target = original.retaining_address(&current);
            if current == target {
                return Ok(None);
            }
            Ok(Some((
                key.clone(),
                service.to_owned(),
                kind,
                current,
                target,
            )))
        })();
        match change {
            Ok(Some(change)) => plan.push(change),
            Ok(None) => {}
            Err(err) => error = Some(err),
        }
    }
    if !plan.is_empty() {
        for (key, _, _, current, target) in &plan {
            snapshot
                .macos_writes
                .insert(key.clone(), current.transition_to(target));
        }
        save(&snapshot, path)?;
        for (_, service, kind, current, target) in plan {
            if let Err(err) = kind.apply_value(&service, &current, &target) {
                error = Some(err);
            }
        }
    }
    error.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_and_restore_preserve_an_external_edit_until_explicit_enable() {
        let kind = KINDS[0];
        let key = "Fixture/webproxy";
        let before = "Enabled: No\nServer: \nPort: 0";
        let manual = "Enabled: Yes\nServer: example.invalid\nPort: 8080";
        let original = kind.parse(before).unwrap();
        let external = kind.parse(manual).unwrap();
        let local = targets("localhost", 2080, None, None)["webproxy"].clone();
        let mut snapshot = ProxySnapshot {
            backend: ProxyBackend::MacOs.label().into(),
            entries: BTreeMap::from([(key.into(), before.into())]),
            macos_writes: BTreeMap::new(),
            applied_at: 0,
            http_host: "localhost".into(),
            http_port: 2080,
            pac_url: None,
        };
        assert!(can_restore(&snapshot, key, kind, &local, &original));
        assert!(!can_restore(&snapshot, key, kind, &external, &original));
        snapshot
            .macos_writes
            .insert(key.into(), vec![local.clone()]);
        assert!(!prepare_apply(
            &mut snapshot,
            key,
            manual,
            &external,
            &local,
            true
        ));
        assert!(!can_restore(&snapshot, key, kind, &external, &original));
        assert_eq!(snapshot.entries[key], before);
        assert!(prepare_apply(
            &mut snapshot,
            key,
            manual,
            &external,
            &local,
            false
        ));
        assert_eq!(snapshot.entries[key], manual);
        assert!(can_restore(&snapshot, key, kind, &local, &external));
        // A crash after restoring a dormant address may leave its switch on.
        let disabled = Value {
            enabled: false,
            address: external.address.clone(),
        };
        assert!(local.transition_to(&disabled).contains(&external));
        let manually_disabled = Value {
            enabled: false,
            address: local.address.clone(),
        };
        assert!(!can_restore(
            &snapshot,
            key,
            kind,
            &manually_disabled,
            &external
        ));
    }

    #[test]
    fn restoring_inactive_addresses_sets_the_switch_last() {
        for kind in KINDS {
            let saved = if kind.key == "autoproxy" {
                "URL: http://example.invalid/proxy.pac\nEnabled: No"
            } else {
                "Enabled: No\nServer: example.invalid\nPort: 8080"
            };
            let target = kind.parse(saved).unwrap();
            let current = Value {
                enabled: true,
                address: vec!["localhost".into(), "7890".into()],
            };
            let mut commands = Vec::new();
            kind.write("Fixture", &current, &target, |args| {
                commands.push(args.join(" "));
                Ok(())
            })
            .unwrap();
            assert_eq!(commands.len(), 2);
            assert_eq!(commands[1], format!("{} Fixture off", kind.state));
            assert!(
                kind.write("Fixture", &target, &target, |_| panic!("unchanged"))
                    .is_ok()
            );
        }
    }
}
