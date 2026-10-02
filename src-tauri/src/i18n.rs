use crate::ui_locale::LOCALE_ZH_CN;

pub fn tray_label(locale: &str, key: &str) -> &'static str {
    match (locale, key) {
        (LOCALE_ZH_CN, "read_selection") => "朗读选中文字",
        (LOCALE_ZH_CN, "translate_selection") => "翻译并朗读",
        (LOCALE_ZH_CN, "stop_reading") => "停止朗读",
        (LOCALE_ZH_CN, "show_main_window") => "显示 VoiceX",
        (LOCALE_ZH_CN, "quit_app") => "退出 VoiceX",
        (_, "read_selection") => "Read Selection",
        (_, "translate_selection") => "Translate and Read",
        (_, "stop_reading") => "Stop Reading",
        (_, "show_main_window") => "Show VoiceX",
        (_, "quit_app") => "Quit VoiceX",
        _ => "",
    }
}
