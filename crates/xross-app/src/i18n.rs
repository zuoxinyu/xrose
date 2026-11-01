//! Two-language UI helper. Each call site supplies both strings, which keeps
//! translations next to the layout they describe instead of in a distant table.

use gpui_kit::SharedString;
use xross_core::settings::Language;

pub fn tr(lang: Language, zh: &'static str, en: &'static str) -> SharedString {
    SharedString::from(match lang {
        Language::Zh => zh,
        Language::En => en,
    })
}
