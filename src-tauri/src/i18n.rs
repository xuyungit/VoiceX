use crate::ui_locale::{resolve_ui_locale, LOCALE_ZH_CN};

fn tray_label(locale: &str, key: &str) -> &'static str {
    match (locale, key) {
        (LOCALE_ZH_CN, "show_main_window") => "显示 VoiceX",
        (LOCALE_ZH_CN, "quit_app") => "退出 VoiceX",
        (_, "show_main_window") => "Show VoiceX",
        (_, "quit_app") => "Quit VoiceX",
        _ => "",
    }
}

pub fn apply_tray_menu(app: &tauri::AppHandle, preferred_language: &str) -> Result<(), String> {
    let locale = resolve_ui_locale(preferred_language);

    let menu = tauri::menu::MenuBuilder::new(app)
        .text("show_main_window", tray_label(&locale, "show_main_window"))
        .separator()
        .text("quit_app", tray_label(&locale, "quit_app"))
        .build()
        .map_err(|err| err.to_string())?;

    let Some(tray) = app.tray_by_id("main") else {
        return Err("Tray icon 'main' not found".to_string());
    };

    tray.set_menu(Some(menu)).map_err(|err| err.to_string())
}
