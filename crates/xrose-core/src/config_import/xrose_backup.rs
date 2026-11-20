//! Xrose JSON backup (`BackupBundle` v1) parser.

use serde_json::Value;

use crate::error::{Error, Result};
use crate::model::{AppModel, WorkMode};
use crate::storage::{BackupBundle, migrate_backup_model};
use crate::subscription::Subscription;

use super::{ConfigFormat, ParsedConfig, ParsedProfile, ParsedRouting, safe_http_subscription_url};

pub(crate) fn parse(value: &Value) -> Result<ParsedConfig> {
    let bundle: BackupBundle = serde_json::from_value(value.clone())
        .map_err(|err| Error::config(format!("Xrose 备份格式无效：{err}")))?;
    if bundle.format != 1 {
        return Err(Error::config(format!(
            "不支持的 Xrose 备份版本：{}",
            bundle.format
        )));
    }

    let mut warnings = Vec::new();
    let mut profiles = Vec::new();
    for mut item in bundle.profiles {
        migrate_backup_model(&mut item.model_json)?;
        let model: AppModel = serde_json::from_value(item.model_json)
            .map_err(|err| Error::config(format!("备份配置档“{}”解析失败：{err}", item.name)))?;
        let mut profile_warnings = Vec::new();
        let nodes: Vec<_> = model
            .nodes
            .into_iter()
            .filter(|node| {
                if node.is_mesh_gateway() {
                    profile_warnings.push("已跳过备份中的本机组网网关虚拟节点".into());
                    false
                } else {
                    true
                }
            })
            .collect();
        let valid_ids: std::collections::BTreeSet<_> =
            nodes.iter().map(|n| n.id.as_str()).collect();
        let mut groups = model.groups;
        for group in &mut groups {
            group.members.retain(|id| valid_ids.contains(id.as_str()));
            group.repair();
        }
        let work_mode = match model.work_mode {
            WorkMode::Mesh => {
                profile_warnings.push(
                    "备份处于组网模式，因组网设备绑定本机环境，导入后不自动切换至组网模式".into(),
                );
                None
            }
            mode => Some(mode),
        };
        let routing = ParsedRouting {
            preset: model.routing.preset,
            rules: model.routing.rules,
            rule_groups: model.routing.rule_groups,
            final_action: Some(model.routing.final_action),
            rule_sets: model.routing.rule_sets,
            domain_strategy: Some(model.routing.domain_strategy),
            rule_set_source: Some(model.routing.rule_set_source),
            custom_rule_set_template: Some(model.routing.custom_rule_set_template),
            preserve_structure: true,
        };
        profiles.push(ParsedProfile {
            name: if item.name.trim().is_empty() {
                model.name
            } else {
                item.name
            },
            work_mode,
            nodes,
            groups,
            default_group: model.default_group,
            routing: Some(routing),
            dns: Some(model.dns),
            warnings: profile_warnings,
        });
    }

    let mut subscriptions = Vec::new();
    for sub in bundle.subscriptions {
        if !safe_http_subscription_url(&sub.url) {
            warnings.push(format!(
                "已跳过无效订阅“{}”：地址需为 HTTP(S) 链接",
                sub.name
            ));
            continue;
        }
        let mut s = Subscription::new(sub.name, sub.url);
        if !sub.user_agent.trim().is_empty()
            && sub.user_agent != crate::subscription::DEFAULT_USER_AGENT
        {
            s.user_agent = sub.user_agent;
        }
        s.headers = sub.headers;
        subscriptions.push(s);
    }

    Ok(ParsedConfig {
        format: ConfigFormat::XroseBackup,
        exported_at: (!bundle.exported_at.trim().is_empty()).then_some(bundle.exported_at),
        profiles,
        subscriptions,
        warnings,
    })
}
