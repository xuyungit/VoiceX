//! The tray menu: the menu-bar extra on macOS, the notification-area icon on
//! Windows.
//!
//! Besides showing the window and quitting, it offers the two selected-text
//! reads, for when there is a mouse at hand and no keyboard. They read the
//! selection of the application the user was in before opening the menu —
//! which on Windows is no longer in front by then (see
//! [`crate::selection::SelectionTarget::BeforeTrayMenu`]).

use std::sync::Mutex;

use tauri::menu::{MenuBuilder, MenuItem};
use tauri::{AppHandle, Manager, Wry};

use crate::commands::settings::AppSettings;
use crate::i18n::tray_label;
use crate::tts::controller::ReadKind;
use crate::tts::TtsController;
use crate::ui_locale::resolve_ui_locale;

const READ_SELECTION: &str = "read_selection";
const TRANSLATE_SELECTION: &str = "translate_selection";
const STOP_READING: &str = "stop_reading";
const SHOW_MAIN_WINDOW: &str = "show_main_window";
const QUIT_APP: &str = "quit_app";

/// Reading needs a selection reader, which exists on macOS and Windows only.
const SELECTION_READING_SUPPORTED: bool = cfg!(any(target_os = "macos", target_os = "windows"));

/// The reading items of the attached menu, kept so they can follow the
/// reading session without the menu being rebuilt.
struct ReadingItems {
    /// `None` when that read is switched off in the settings.
    read: Option<MenuItem<Wry>>,
    translate: Option<MenuItem<Wry>>,
    stop: MenuItem<Wry>,
}

static READING_ITEMS: Mutex<Option<ReadingItems>> = Mutex::new(None);

/// Build the menu for `settings` and attach it to the tray icon. Called at
/// startup and whenever a setting it depends on changes: the UI language,
/// and the two reading switches — a read that is switched off is left out of
/// the menu, the way its hotkey is left unbound.
pub fn apply_menu(app: &AppHandle, settings: &AppSettings) -> Result<(), String> {
    let locale = resolve_ui_locale(&settings.ui_language);
    let label = |key: &str| tray_label(&locale, key);
    let item = |id: &str| {
        MenuItem::with_id(app, id, label(id), true, None::<&str>).map_err(|err| err.to_string())
    };

    let read = (SELECTION_READING_SUPPORTED && settings.tts_enabled)
        .then(|| item(READ_SELECTION))
        .transpose()?;
    let translate = (SELECTION_READING_SUPPORTED && settings.tts_translate_enabled)
        .then(|| item(TRANSLATE_SELECTION))
        .transpose()?;
    let reading = if read.is_some() || translate.is_some() {
        Some(ReadingItems {
            read,
            translate,
            stop: item(STOP_READING)?,
        })
    } else {
        None
    };

    let mut builder = MenuBuilder::new(app);
    if let Some(items) = reading.as_ref() {
        if let Some(read) = items.read.as_ref() {
            builder = builder.item(read);
        }
        if let Some(translate) = items.translate.as_ref() {
            builder = builder.item(translate);
        }
        builder = builder.item(&items.stop).separator();
    }
    let menu = builder
        .text(SHOW_MAIN_WINDOW, label(SHOW_MAIN_WINDOW))
        .separator()
        .text(QUIT_APP, label(QUIT_APP))
        .build()
        .map_err(|err| err.to_string())?;

    let Some(tray) = app.tray_by_id("main") else {
        return Err("Tray icon 'main' not found".to_string());
    };
    tray.set_menu(Some(menu)).map_err(|err| err.to_string())?;

    if let Ok(mut slot) = READING_ITEMS.lock() {
        *slot = reading;
    }
    // A read may be under way while the menu is rebuilt.
    reading_state_changed(app);
    Ok(())
}

/// Grey out whichever of reading and stopping does not apply right now.
///
/// Called from any thread, any number of times, possibly out of order. Each
/// call reads the session state when it runs on the main thread rather than
/// being told it, so whichever runs last — after the last change — is right.
pub fn reading_state_changed(app: &AppHandle) {
    let handle = app.clone();
    let queued = app.run_on_main_thread(move || {
        let active = handle.state::<TtsController>().is_active();
        let Ok(slot) = READING_ITEMS.lock() else {
            return;
        };
        let Some(items) = slot.as_ref() else {
            return;
        };
        for item in items.read.iter().chain(items.translate.iter()) {
            if let Err(err) = item.set_enabled(!active) {
                log::warn!("Failed to update a tray reading item: {err}");
            }
        }
        if let Err(err) = items.stop.set_enabled(active) {
            log::warn!("Failed to update the tray stop item: {err}");
        }
    });
    if let Err(err) = queued {
        log::warn!("Failed to queue the tray menu update: {err}");
    }
}

/// Runs on the main thread, from the menu's event handler.
pub fn handle_menu_event(app: &AppHandle, id: &str) {
    match id {
        READ_SELECTION => run_off_main_thread(app, |tts| tts.handle_menu_read(ReadKind::Read)),
        TRANSLATE_SELECTION => {
            run_off_main_thread(app, |tts| tts.handle_menu_read(ReadKind::Translate))
        }
        STOP_READING => run_off_main_thread(app, |tts| tts.handle_menu_stop()),
        SHOW_MAIN_WINDOW => show_main_window(app),
        QUIT_APP => app.exit(0),
        _ => {}
    }
}

/// Starting and stopping a read block (the system voice's stop waits on the
/// main thread), so they are handed off the way the hotkey hands them to its
/// worker.
fn run_off_main_thread(app: &AppHandle, action: impl FnOnce(&TtsController) + Send + 'static) {
    let tts = app.state::<TtsController>().inner().clone();
    tauri::async_runtime::spawn_blocking(move || action(&tts));
}

fn show_main_window(app: &AppHandle) {
    let Some(main) = app.get_webview_window("main") else {
        return;
    };
    let _ = main.show();
    let _ = main.set_focus();

    // Under Accessory policy the app has no Dock presence,
    // so we must explicitly activate it to bring the window
    // to the foreground.
    #[cfg(target_os = "macos")]
    {
        let _ = main.run_on_main_thread(|| {
            use objc2::MainThreadMarker;
            use objc2_app_kit::NSApplication;
            // Safe: run_on_main_thread guarantees we are on the main thread.
            let mtm = unsafe { MainThreadMarker::new_unchecked() };
            let ns_app = NSApplication::sharedApplication(mtm);
            ns_app.activate();
        });
    }
}
