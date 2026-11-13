//! Observe the effective macOS proxy configuration on a serial dispatch queue.

use std::{collections::BTreeMap, ptr, sync::Arc};

use dispatch2::{DispatchQueue, DispatchRetained};
use parking_lot::RwLock;
use system_configuration::{
    core_foundation::{
        array::CFArray,
        base::{CFType, TCFType},
        boolean::CFBoolean,
        dictionary::CFDictionary,
        number::CFNumber,
        string::CFString,
    },
    dynamic_store::{SCDynamicStore, SCDynamicStoreBuilder, SCDynamicStoreCallBackContext},
    sys::dynamic_store::SCDynamicStoreSetDispatchQueue,
};

use super::{
    SystemProxyStatus,
    macos::{self, Value},
};

type Proxies = BTreeMap<String, Value>;

#[derive(Default)]
pub(crate) struct Observation {
    started: bool,
    effective: Option<Proxies>,
    expected: Option<Proxies>,
}

impl Observation {
    pub(crate) fn expect(
        &mut self,
        host: &str,
        port: u16,
        socks: Option<(&str, u16)>,
        pac: Option<&str>,
    ) {
        self.expected = Some(macos::targets(host, port, socks, pac));
    }

    pub(crate) fn clear_expected(&mut self) {
        self.expected = None;
    }

    pub(crate) fn status(&self) -> Option<SystemProxyStatus> {
        if !self.started {
            return None;
        }
        let Some(effective) = &self.effective else {
            return Some(SystemProxyStatus::Unknown);
        };
        if effective.is_empty() {
            return Some(SystemProxyStatus::Off);
        }
        let Some(expected) = &self.expected else {
            return Some(SystemProxyStatus::External);
        };
        let matched = effective
            .iter()
            .filter(|(key, value)| expected.get(*key) == Some(*value))
            .count();
        let expected_count = expected.values().filter(|v| v.enabled).count();
        Some(if matched == 0 {
            SystemProxyStatus::External
        } else if matched == expected_count && matched == effective.len() {
            SystemProxyStatus::Xrose
        } else {
            SystemProxyStatus::Partial
        })
    }
}

fn effective_proxies(dict: &CFDictionary<CFString, CFType>) -> Proxies {
    let enabled = |key: &str| {
        dict.find(CFString::from(key)).is_some_and(|v| {
            v.downcast::<CFNumber>()
                .and_then(|n| n.to_i32())
                .is_some_and(|n| n != 0)
                || v.downcast::<CFBoolean>().is_some_and(bool::from)
        })
    };
    let string = |key: &str| {
        dict.find(CFString::from(key))
            .and_then(|v| v.downcast::<CFString>())
            .map(|s| s.to_string())
    };
    let port = |key: &str| {
        dict.find(CFString::from(key))
            .and_then(|v| v.downcast::<CFNumber>())
            .and_then(|n| n.to_i32())
            .map(|n| n.to_string())
    };
    let mut proxies = Proxies::new();
    for (kind, prefix) in [
        ("webproxy", "HTTP"),
        ("securewebproxy", "HTTPS"),
        ("socksproxy", "SOCKS"),
        ("autoproxy", "ProxyAutoConfig"),
        ("discovery", "ProxyAutoDiscovery"),
        ("ftp", "FTP"),
        ("rtsp", "RTSP"),
        ("gopher", "Gopher"),
    ] {
        if enabled(&format!("{prefix}Enable")) {
            let address = match kind {
                "autoproxy" => string("ProxyAutoConfigURLString").into_iter().collect(),
                "discovery" => Vec::new(),
                _ => string(&format!("{prefix}Proxy"))
                    .zip(port(&format!("{prefix}Port")))
                    .map(|(h, p)| vec![h, p])
                    .unwrap_or_default(),
            };
            proxies.insert(
                kind.to_owned(),
                Value {
                    enabled: true,
                    address,
                },
            );
        }
    }
    proxies
}

fn update(store: Option<&SCDynamicStore>, observation: &RwLock<Observation>) {
    let effective = store
        .and_then(SCDynamicStore::get_proxies)
        .as_ref()
        .map(effective_proxies);
    observation.write().effective = effective;
}

pub(crate) struct Observer {
    store: SCDynamicStore,
}

impl Drop for Observer {
    fn drop(&mut self) {
        // Detach delivery before releasing our store reference.
        unsafe {
            SCDynamicStoreSetDispatchQueue(self.store.as_concrete_TypeRef(), ptr::null_mut());
        }
    }
}

pub(crate) fn subscribe(observation: Arc<RwLock<Observation>>) -> Option<Observer> {
    observation.write().started = true;
    let store = SCDynamicStoreBuilder::new("Xrose.SystemProxy")
        .callback_context(SCDynamicStoreCallBackContext {
            info: observation.clone(),
            callout: |store, _, state| update(Some(&store), state),
        })
        .build()?;
    let keys = CFArray::from_CFTypes(&[CFString::from("State:/Network/Global/Proxies")]);
    if !store.set_notification_keys(&keys, &CFArray::<CFString>::from_CFTypes(&[])) {
        return None;
    }
    let queue = DispatchQueue::new("Xrose.SystemProxy", None);
    let ok = unsafe {
        SCDynamicStoreSetDispatchQueue(
            store.as_concrete_TypeRef(),
            DispatchRetained::as_ptr(&queue).as_ptr().cast(),
        )
    };
    if ok == 0 {
        return None;
    }
    // Read once on the notification queue so the initial value and later events
    // are delivered in order. The UI never waits for a SystemConfiguration read.
    queue.exec_async(move || {
        let reader = SCDynamicStoreBuilder::new("Xrose.SystemProxy.Read").build();
        update(reader.as_ref(), &observation);
    });
    Some(Observer { store })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_changes_update_ownership_for_each_protocol() {
        let mut state = Observation::default();
        assert_eq!(state.status(), None);
        state.started = true;
        assert_eq!(state.status(), Some(SystemProxyStatus::Unknown));
        let off = CFDictionary::from_CFType_pairs(&[
            (CFString::from("HTTPEnable"), CFNumber::from(0).as_CFType()),
            (
                CFString::from("HTTPProxy"),
                CFString::from("localhost").as_CFType(),
            ),
        ]);
        state.effective = Some(effective_proxies(&off));
        assert_eq!(state.status(), Some(SystemProxyStatus::Off));
        let dict = CFDictionary::from_CFType_pairs(&[
            (
                CFString::from("HTTPEnable"),
                CFBoolean::true_value().as_CFType(),
            ),
            (
                CFString::from("HTTPProxy"),
                CFString::from("localhost").as_CFType(),
            ),
            (CFString::from("HTTPPort"), CFNumber::from(2080).as_CFType()),
            (CFString::from("__SCOPED__"), off.as_CFType()),
        ]);
        state.effective = Some(effective_proxies(&dict));
        assert_eq!(state.status(), Some(SystemProxyStatus::External));
        state.expect("localhost", 2080, None, None);
        assert_eq!(state.status(), Some(SystemProxyStatus::Partial));
        let https = state.expected.as_ref().unwrap()["securewebproxy"].clone();
        let actual = state.effective.as_mut().unwrap();
        actual.insert("securewebproxy".into(), https);
        assert_eq!(state.status(), Some(SystemProxyStatus::Xrose));
        state.expect("localhost", 2081, None, None);
        assert_eq!(state.status(), Some(SystemProxyStatus::External));
        state.effective = Some(effective_proxies(&off));
        assert_eq!(state.status(), Some(SystemProxyStatus::Off));
    }

    #[test]
    fn pac_and_discovery_have_distinct_ownership() {
        let mut state = Observation {
            started: true,
            ..Default::default()
        };
        let url = "http://localhost:2082/proxy.pac";
        let mut fields = vec![
            (
                CFString::from("ProxyAutoConfigEnable"),
                CFNumber::from(1).as_CFType(),
            ),
            (
                CFString::from("ProxyAutoConfigURLString"),
                CFString::from(url).as_CFType(),
            ),
        ];
        state.expect("localhost", 2080, None, Some(url));
        state.effective = Some(effective_proxies(&CFDictionary::from_CFType_pairs(&fields)));
        assert_eq!(state.status(), Some(SystemProxyStatus::Xrose));
        fields.push((
            CFString::from("ProxyAutoDiscoveryEnable"),
            CFNumber::from(1).as_CFType(),
        ));
        state.effective = Some(effective_proxies(&CFDictionary::from_CFType_pairs(&fields)));
        assert_eq!(state.status(), Some(SystemProxyStatus::Partial));
        state.clear_expected();
        assert_eq!(state.status(), Some(SystemProxyStatus::External));
    }
}
