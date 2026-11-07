//! Two-language UI helper. Each call site supplies both strings, which keeps
//! translations next to the layout they describe instead of in a distant table.

use gpui_kit::SharedString;
use xrose_core::settings::Language;

pub fn tr(lang: Language, zh: &'static str, en: &'static str) -> SharedString {
    SharedString::from(match lang {
        Language::Zh => zh,
        Language::En => en,
    })
}

/// Relative dates for the two supported locales, without mixed-language suffixes.
pub fn relative_time(lang: Language, seconds: i64) -> String {
    let seconds = seconds.max(0);
    if seconds < 60 {
        return tr(lang, "刚刚", "Just now").to_string();
    }
    let (n, zh, en) = if seconds < 3600 {
        (seconds / 60, "分钟", "minute")
    } else if seconds < 86400 {
        (seconds / 3600, "小时", "hour")
    } else {
        (seconds / 86400, "天", "day")
    };
    if lang == Language::Zh {
        format!("{n} {zh}前")
    } else {
        format!("{n} {en}{} ago", if n == 1 { "" } else { "s" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn relative_time_handles_boundaries_and_plurals() {
        assert_eq!(relative_time(Language::Zh, -1), "刚刚");
        assert_eq!(relative_time(Language::Zh, 59), "刚刚");
        assert_eq!(relative_time(Language::Zh, 60), "1 分钟前");
        assert_eq!(relative_time(Language::En, 60), "1 minute ago");
        assert_eq!(relative_time(Language::En, 120), "2 minutes ago");
        assert_eq!(relative_time(Language::Zh, 3600), "1 小时前");
        assert_eq!(relative_time(Language::En, 86400), "1 day ago");
    }
}
