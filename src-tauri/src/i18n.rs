use crate::ui_locale::LOCALE_ZH_CN;

pub fn tray_label(locale: &str, key: &str) -> &'static str {
    match (locale, key) {
        (LOCALE_ZH_CN, "read_selection") => "朗读原文",
        (LOCALE_ZH_CN, "translate_selection") => "朗读译文",
        (LOCALE_ZH_CN, "stop_reading") => "停止朗读",
        (LOCALE_ZH_CN, "copy_last_dictation") => "复制上次听写",
        (LOCALE_ZH_CN, "show_main_window") => "打开 VoiceX",
        (LOCALE_ZH_CN, "quit_app") => "退出 VoiceX",
        (LOCALE_ZH_CN, "update_available") => "有可用更新…",
        (_, "read_selection") => "Read Aloud",
        (_, "translate_selection") => "Read Translation",
        (_, "stop_reading") => "Stop Reading",
        (_, "copy_last_dictation") => "Copy Last Dictation",
        (_, "show_main_window") => "Open VoiceX",
        (_, "quit_app") => "Quit VoiceX",
        (_, "update_available") => "Update Available…",
        _ => "",
    }
}
