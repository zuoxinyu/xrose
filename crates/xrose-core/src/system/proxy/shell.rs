//! Generate session-scoped shell variables for the saved local mixed listener.

use crate::{Error, Result, Settings, model::inbound::mixed_http_port, util::host_port};

pub struct ProxyEnvCommand {
    pub text: String,
    pub requires_credentials: bool,
}

pub(crate) fn for_default_shell(settings: &Settings) -> Result<ProxyEnvCommand> {
    #[cfg(windows)]
    let shell = "powershell";
    #[cfg(not(windows))]
    let login_shell = std::env::var_os("SHELL");
    #[cfg(not(windows))]
    let shell = login_shell
        .as_deref()
        .and_then(|s| std::path::Path::new(s).file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("sh");
    generate(settings, shell)
}

fn generate(settings: &Settings, shell: &str) -> Result<ProxyEnvCommand> {
    let port = settings.mixed_port;
    if port == 0 {
        return Err(Error::config("请先设置有效的混合端口"));
    }
    let http_port = mixed_http_port(settings.core, port)?;
    let listen = settings.listen_host.trim();
    let host = if settings.allow_lan || listen.is_empty() {
        "127.0.0.1".to_owned()
    } else {
        // URLs embedded in shell source must contain only host characters.
        let invalid = || Error::config("监听地址无效，请检查混合入站的地址");
        let host = match listen.parse::<std::net::Ipv6Addr>() {
            Ok(ip) => url::Host::Ipv6(ip),
            Err(_) => url::Host::parse(listen).map_err(|_| invalid())?,
        };
        match host {
            url::Host::Ipv4(ip) if ip.is_unspecified() => "127.0.0.1".into(),
            url::Host::Ipv6(ip) if ip.is_unspecified() => "::1".into(),
            url::Host::Domain(ref name)
                if !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b)) =>
            {
                return Err(invalid());
            }
            host => host.to_string(),
        }
    };
    let requires_credentials =
        settings.allow_lan && !settings.separate_lan_port && settings.lan_auth;
    let auth = if requires_credentials {
        "USER:PASSWORD@"
    } else {
        ""
    };
    let http = format!("http://{auth}{}", host_port(&host, http_port));
    let socks = format!("socks5h://{auth}{}", host_port(&host, port));
    let mut lines = Vec::new();
    for (name, url) in [
        ("http_proxy", &http),
        ("https_proxy", &http),
        ("all_proxy", &socks),
    ] {
        let upper = name.to_ascii_uppercase();
        match shell {
            "powershell" | "pwsh" => {
                lines.push(format!("$env:{upper} = '{url}'"));
                if !cfg!(windows) {
                    lines.push(format!("$env:{name} = $env:{upper}"));
                }
            }
            "fish" => {
                lines.push(format!("set -gx {name} '{url}'"));
                lines.push(format!("set -gx {upper} \"${name}\""));
            }
            "csh" | "tcsh" => {
                lines.push(format!("setenv {name} '{url}'"));
                lines.push(format!("setenv {upper} \"${name}\""));
            }
            "nu" => {
                lines.push(format!("$env.{name} = '{url}'"));
                lines.push(format!("$env.{upper} = $env.{name}"));
            }
            _ => {
                lines.push(format!("export {name}='{url}'"));
                lines.push(format!("export {upper}=\"${name}\""));
            }
        }
    }
    Ok(ProxyEnvCommand {
        text: lines.join(if cfg!(windows) { "\r\n" } else { "\n" }),
        requires_credentials,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CoreKind;

    #[test]
    fn environment_matches_each_cores_local_listener() {
        let mut s = Settings {
            mixed_port: 18080,
            core: CoreKind::Xray,
            listen_host: "[::]".into(),
            ..Default::default()
        };
        let cmd = generate(&s, "pwsh").unwrap();
        assert!(cmd.text.contains("$env:HTTP_PROXY = 'http://[::1]:18081'"));
        assert!(cmd.text.contains("$env:HTTPS_PROXY = 'http://[::1]:18081'"));
        assert!(
            cmd.text
                .contains("$env:ALL_PROXY = 'socks5h://[::1]:18080'")
        );
        s.mixed_port = u16::MAX;
        assert!(generate(&s, "pwsh").is_err());
        s.core = CoreKind::SingBox;
        let cmd = generate(&s, "bash").unwrap();
        assert!(cmd.text.contains("http://[::1]:65535"));
        assert!(cmd.text.contains("socks5h://[::1]:65535"));
        s.listen_host = "bad'host".into();
        assert!(generate(&s, "bash").is_err());
    }

    #[test]
    fn lan_auth_only_requires_placeholders_on_the_shared_port() {
        let mut s = Settings {
            mixed_port: 18080,
            lan_port: 19090,
            allow_lan: true,
            lan_auth: true,
            separate_lan_port: false,
            ..Default::default()
        };
        let cmd = generate(&s, "fish").unwrap();
        assert!(cmd.requires_credentials);
        assert!(
            cmd.text
                .contains("set -gx http_proxy 'http://USER:PASSWORD@127.0.0.1:18080'")
        );
        s.separate_lan_port = true;
        let cmd = generate(&s, "tcsh").unwrap();
        assert!(!cmd.requires_credentials);
        assert!(
            cmd.text
                .contains("setenv http_proxy 'http://127.0.0.1:18080'")
        );
        assert!(!cmd.text.contains('@'));
    }

    #[cfg(unix)]
    #[test]
    fn posix_command_exports_lowercase_and_uppercase_to_children() {
        let s = Settings {
            mixed_port: 18080,
            core: CoreKind::Xray,
            ..Default::default()
        };
        let cmd = generate(&s, "zsh").unwrap().text;
        let output = std::process::Command::new("/bin/sh")
            .args(["-c", &(cmd + "\nenv")])
            .env_clear()
            .output()
            .unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8(output.stdout).unwrap();
        for (name, url) in [
            ("http_proxy", "http://127.0.0.1:18081"),
            ("https_proxy", "http://127.0.0.1:18081"),
            ("all_proxy", "socks5h://127.0.0.1:18080"),
        ] {
            for key in [name.to_string(), name.to_ascii_uppercase()] {
                assert!(stdout.lines().any(|line| line == format!("{key}={url}")));
            }
        }
    }
}
