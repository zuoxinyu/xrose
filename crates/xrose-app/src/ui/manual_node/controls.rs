//! Choice controls for fixed protocol values and editable, ordered presets.

use super::{AppView, ChoiceGroup, FieldWidth, Palette, dialog_form_row};
use crate::i18n::tr;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{IconName, Sizable};
use gpui_kit::*;
use xrose_core::settings::Language;

pub(super) fn fixed_choice(
    id: &'static str,
    values: &'static [&'static str],
    current: &str,
    empty_label: SharedString,
    cx: &Context<AppView>,
    set: impl Fn(&mut AppView, &'static str) + 'static,
) -> ChoiceGroup {
    let mut labels: Vec<_> = values
        .iter()
        .map(|&value| match value {
            "" => empty_label.clone(),
            "xtls-rprx-vision" => "Vision".into(),
            "xtls-rprx-vision-udp443" => "Vision (UDP 443)".into(),
            _ => value.into(),
        })
        .collect();
    // Keep an imported value visible until the user chooses a supported replacement.
    let selected = values
        .iter()
        .position(|&v| v == current)
        .unwrap_or_else(|| {
            labels.push(current.to_string().into());
            labels.len() - 1
        });
    ChoiceGroup::new(id, labels, selected)
        .small()
        .on_change(cx.listener(move |this, index, _, cx| {
            if let Some(&value) = values.get(*index) {
                set(this, value);
                this.manual_error = None;
                cx.notify();
            }
        }))
}

#[derive(Clone, Copy)]
pub(super) enum Presets {
    Single(&'static [&'static str]),
    /// Delimiters match the parser of the corresponding draft field.
    Ordered(&'static [&'static str], &'static [char]),
}

impl Presets {
    fn selected(self, current: &str, option: &str) -> bool {
        match self {
            Self::Single(_) => current.trim() == option,
            Self::Ordered(_, delimiters) => current.split(delimiters).any(|v| v.trim() == option),
        }
    }

    fn pick(self, current: &str, option: &str) -> String {
        let Self::Ordered(_, delimiters) = self else {
            return option.to_owned();
        };
        let mut values: Vec<_> = current
            .split(delimiters)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .collect();
        if values.contains(&option) {
            values.retain(|&v| v != option);
        } else {
            values.push(option);
        }
        values.join(", ")
    }
}

pub(super) fn preset_field(
    p: &Palette,
    lang: Language,
    label: SharedString,
    input: &Entity<InputState>,
    presets: Presets,
) -> Div {
    let target = input.clone();
    let hint = tr(lang, "选择常用值", "Choose a preset");
    let menu = Button::new(("node-presets", input.entity_id()))
        .ghost()
        .xsmall()
        .icon(IconName::ChevronDown)
        .accessibility_label(format!("{label} · {hint}"))
        .tooltip(hint)
        .dropdown_menu(move |mut menu, _, cx| {
            let current = target.read(cx).value();
            let options = match presets {
                Presets::Single(options) | Presets::Ordered(options, _) => options,
            };
            for &option in options {
                let target = target.clone();
                menu = menu.item(
                    PopupMenuItem::new(option)
                        .checked(presets.selected(&current, option))
                        .on_click(move |_, window, cx| {
                            target.update(cx, |state, cx| {
                                let value = presets.pick(&state.value(), option);
                                state.set_value(value, window, cx);
                            });
                        }),
                );
            }
            menu
        });
    dialog_form_row(
        p,
        label,
        "",
        Input::new(input).suffix(menu).cleanable(true),
        FieldWidth::Full.into(),
    )
}

pub(super) fn integer_input(
    initial: u16,
    window: &mut Window,
    cx: &mut Context<AppView>,
) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx)
            .validate(|text, _| text.bytes().all(|byte| byte.is_ascii_digit()))
            .placeholder(initial.to_string())
            .default_value(initial.to_string())
    })
}

pub(super) fn secret_field(p: &Palette, label: SharedString, input: &Entity<InputState>) -> Div {
    dialog_form_row(
        p,
        label,
        "",
        Input::new(input).mask_toggle(),
        FieldWidth::Full.into(),
    )
}

#[cfg(test)]
mod tests {
    use super::Presets;

    #[test]
    fn ordered_presets_keep_custom_entries_and_preference_order() {
        let alpn = Presets::Ordered(&["h2", "http/1.1", "h3"], &[',', ' ', ';']);
        let original = "custom/1;http/1.1 h2";
        assert!(alpn.selected(original, "h2"));
        assert_eq!(alpn.pick(original, "h2"), "custom/1, http/1.1");
        assert_eq!(alpn.pick(original, "h3"), "custom/1, http/1.1, h2, h3");
        assert_eq!(alpn.pick("h3", "h3"), "");

        let vpn = Presets::Ordered(&["AES-128-GCM"], &[',']);
        assert_eq!(
            vpn.pick("custom cipher", "AES-128-GCM"),
            "custom cipher, AES-128-GCM"
        );
        assert_eq!(
            Presets::Single(&["chrome"]).pick("custom", "chrome"),
            "chrome"
        );
    }
}
