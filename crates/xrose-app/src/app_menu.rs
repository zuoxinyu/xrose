//! macOS application menu bar items and global actions.

use gpui_kit::App;
#[cfg(target_os = "macos")]
use gpui_kit::KeyBinding;
#[cfg(any(target_os = "macos", test))]
use gpui_kit::{Menu, MenuItem};
#[cfg(target_os = "macos")]
use parking_lot::Mutex;
use xrose_core::AppService;
use xrose_core::settings::Language;

#[cfg(any(target_os = "macos", test))]
use crate::i18n::tr;
#[cfg(target_os = "macos")]
use crate::tray::{self, TrayCommand};

#[cfg(any(target_os = "macos", test))]
gpui_kit::actions!(xrose, [About, CheckUpdates, Quit]);

#[cfg(target_os = "macos")]
static INSTALLED_LANGUAGE: Mutex<Option<Language>> = Mutex::new(None);

#[cfg(any(target_os = "macos", test))]
fn build_menus(lang: Language) -> [Menu; 1] {
    [Menu::new("Xrose").items([
        MenuItem::action(tr(lang, "关于", "About"), About),
        MenuItem::action(tr(lang, "检查更新", "Check updates"), CheckUpdates),
        MenuItem::separator(),
        MenuItem::action(tr(lang, "退出", "Quit"), Quit),
    ])]
}

/// Installs the native macOS application menu and global action handlers.
pub fn install(service: &AppService, cx: &mut App) {
    #[cfg(target_os = "macos")]
    {
        cx.on_action(|_: &About, _| tray::queue_command(TrayCommand::ShowAbout));
        cx.on_action(|_: &CheckUpdates, _| tray::queue_command(TrayCommand::ShowUpdates));
        cx.on_action(|_: &Quit, _| tray::queue_command(TrayCommand::Quit));
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
        *INSTALLED_LANGUAGE.lock() = None;
        sync(service.settings().language, cx);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (service, cx);
}

/// Refreshes native macOS menu labels when the active UI language changes.
pub fn sync(lang: Language, cx: &mut App) {
    #[cfg(target_os = "macos")]
    {
        let mut current = INSTALLED_LANGUAGE.lock();
        if *current == Some(lang) {
            return;
        }
        cx.set_menus(build_menus(lang));
        *current = Some(lang);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (lang, cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(menu: &Menu) -> Vec<Option<&str>> {
        menu.items
            .iter()
            .map(|item| match item {
                MenuItem::Separator => None,
                MenuItem::Action { name, .. } => Some(name.as_ref()),
                MenuItem::Submenu(sub) => Some(sub.name.as_ref()),
                MenuItem::SystemMenu(sys) => Some(sys.name.as_ref()),
            })
            .collect()
    }

    #[test]
    fn application_menu_contains_expected_items_in_both_languages() {
        let [zh_menu] = build_menus(Language::Zh);
        assert_eq!(zh_menu.name.as_ref(), "Xrose");
        assert_eq!(
            labels(&zh_menu),
            vec![Some("关于"), Some("检查更新"), None, Some("退出")]
        );

        let [en_menu] = build_menus(Language::En);
        assert_eq!(en_menu.name.as_ref(), "Xrose");
        assert_eq!(
            labels(&en_menu),
            vec![Some("About"), Some("Check updates"), None, Some("Quit")]
        );

        let action_names: Vec<&str> = zh_menu
            .items
            .iter()
            .filter_map(|item| match item {
                MenuItem::Action { action, .. } => Some(action.name()),
                _ => None,
            })
            .collect();
        assert_eq!(
            action_names,
            vec!["xrose::About", "xrose::CheckUpdates", "xrose::Quit"]
        );
    }
}
