//! The tray menu: the menu-bar extra on macOS, the notification-area icon on
//! Windows.
//!
//! Besides showing the window and quitting, it offers the two selected-text
//! reads, for when there is a mouse at hand and no keyboard. They read the
//! selection of the application the user was in before opening the menu —
//! which on Windows is no longer in front by then (see
//! [`crate::selection::SelectionTarget::BeforeTrayMenu`]). While a read is
//! under way, the two reads give way to a single item that stops it.
//!
//! When a check has found a newer version, an item at the top leads to it.
//!
//! Every item answers to a key while the menu is open (see [`item_key`]).

use std::sync::Mutex;

use tauri::menu::{MenuBuilder, MenuItem};
use tauri::{AppHandle, Emitter, Manager, Wry};

use crate::commands::settings::AppSettings;
use crate::i18n::tray_label;
use crate::tts::controller::ReadKind;
use crate::tts::TtsController;
use crate::ui_locale::resolve_ui_locale;
use crate::updater::UpdateService;

const TRAY_ID: &str = "main";

const READ_SELECTION: &str = "read_selection";
const TRANSLATE_SELECTION: &str = "translate_selection";
const STOP_READING: &str = "stop_reading";
const COPY_LAST_DICTATION: &str = "copy_last_dictation";
const SHOW_MAIN_WINDOW: &str = "show_main_window";
const QUIT_APP: &str = "quit_app";
const UPDATE_AVAILABLE: &str = "update_available";

/// Reading needs a selection reader, which exists on macOS and Windows only.
const SELECTION_READING_SUPPORTED: bool = cfg!(any(target_os = "macos", target_os = "windows"));

/// The settings the menu is built from.
#[derive(Clone)]
struct MenuSpec {
    locale: String,
    /// Whether each read is offered; a read that is switched off is left out
    /// of the menu, the way its hotkey is left unbound.
    read: bool,
    translate: bool,
}

struct MenuState {
    spec: MenuSpec,
    /// Whether the attached menu was built for a read under way; `None`
    /// until a menu has been built for the current `spec`.
    built_for_active: Option<bool>,
}

static MENU_STATE: Mutex<Option<MenuState>> = Mutex::new(None);

/// Build the menu for `settings` and attach it to the tray icon. Called at
/// startup and whenever a setting it depends on changes: the UI language,
/// and the two reading switches.
///
/// The menu itself is built on the main thread, like every rebuild that
/// follows the reading session, so that the last build to run always sees
/// both the latest settings and the latest session state.
pub fn apply_menu(app: &AppHandle, settings: &AppSettings) -> Result<(), String> {
    if app.tray_by_id(TRAY_ID).is_none() {
        return Err(format!("Tray icon '{TRAY_ID}' not found"));
    }
    let spec = MenuSpec {
        locale: resolve_ui_locale(&settings.ui_language),
        read: SELECTION_READING_SUPPORTED && settings.tts_enabled,
        translate: SELECTION_READING_SUPPORTED && settings.tts_translate_enabled,
    };
    {
        let mut slot = MENU_STATE.lock().map_err(|err| err.to_string())?;
        *slot = Some(MenuState {
            spec,
            built_for_active: None,
        });
    }
    queue_rebuild(app)
}

/// Swap the reads for the stop item, or back, when a read starts or ends.
///
/// Called from any thread, any number of times, possibly out of order. Each
/// rebuild reads the session state when it runs on the main thread rather
/// than being told it, so whichever runs last — after the last change — is
/// right.
pub fn reading_state_changed(app: &AppHandle) {
    if let Err(err) = queue_rebuild(app) {
        log::warn!("Failed to queue the tray menu update: {err}");
    }
}

fn queue_rebuild(app: &AppHandle) -> Result<(), String> {
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let active = handle.state::<TtsController>().is_active();
        let mut slot = match MENU_STATE.lock() {
            Ok(slot) => slot,
            Err(err) => {
                log::warn!("Failed to lock the tray menu state: {err}");
                return;
            }
        };
        // Nothing to do before the first `apply_menu`.
        let Some(state) = slot.as_mut() else {
            return;
        };
        // Replacing the menu while it is open is best avoided, so a session
        // change that does not change the menu leaves it alone.
        if state.built_for_active == Some(active) {
            return;
        }
        match attach_menu(&handle, &state.spec, active) {
            Ok(()) => state.built_for_active = Some(active),
            Err(err) => log::warn!("Failed to build the tray menu: {err}"),
        }
    })
    .map_err(|err| err.to_string())
}

fn attach_menu(app: &AppHandle, spec: &MenuSpec, reading_active: bool) -> Result<(), String> {
    let label = |key: &str| tray_label(&spec.locale, key);
    let item = |id: &str, text: &str| menu_item(app, id, text);

    let mut builder = MenuBuilder::new(app);
    if let Some(version) = app.state::<UpdateService>().available_version() {
        let text = format!("{} (v{version})", label(UPDATE_AVAILABLE));
        builder = builder.item(&item(UPDATE_AVAILABLE, &text)?).separator();
    }

    let reading_ids: Vec<&str> = if reading_active {
        vec![STOP_READING]
    } else {
        [(spec.read, READ_SELECTION), (spec.translate, TRANSLATE_SELECTION)]
            .into_iter()
            .filter_map(|(offered, id)| offered.then_some(id))
            .collect()
    };
    if !reading_ids.is_empty() {
        for id in reading_ids {
            builder = builder.item(&item(id, label(id))?);
        }
        builder = builder.separator();
    }

    let menu = builder
        .item(&item(COPY_LAST_DICTATION, label(COPY_LAST_DICTATION))?)
        .separator()
        .item(&item(SHOW_MAIN_WINDOW, label(SHOW_MAIN_WINDOW))?)
        .item(&item(QUIT_APP, label(QUIT_APP))?)
        .build()
        .map_err(|err| err.to_string())?;

    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return Err(format!("Tray icon '{TRAY_ID}' not found"));
    };
    tray.set_menu(Some(menu)).map_err(|err| err.to_string())
}

/// The keys an item answers to while the menu is open: a Command shortcut on
/// macOS, shown at the right of the item the way menus show them, and an
/// access key on Windows — underlined, pressed on its own — since a tray
/// menu there does not act on accelerators.
struct ItemKey {
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    macos_accelerator: &'static str,
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    windows_access_key: char,
}

fn item_key(id: &str) -> Option<ItemKey> {
    let (macos_accelerator, windows_access_key) = match id {
        READ_SELECTION => ("Cmd+R", 'R'),
        TRANSLATE_SELECTION => ("Cmd+T", 'T'),
        // ⌘. is how macOS stops things.
        STOP_READING => ("Cmd+Period", 'S'),
        COPY_LAST_DICTATION => ("Cmd+C", 'C'),
        SHOW_MAIN_WINDOW => ("Cmd+O", 'O'),
        QUIT_APP => ("Cmd+Q", 'Q'),
        UPDATE_AVAILABLE => ("Cmd+U", 'U'),
        _ => return None,
    };
    Some(ItemKey {
        macos_accelerator,
        windows_access_key,
    })
}

fn menu_item(app: &AppHandle, id: &str, text: &str) -> Result<MenuItem<Wry>, String> {
    let key = item_key(id);

    #[cfg(target_os = "windows")]
    let text = match key.as_ref() {
        Some(key) => with_access_key(text, key.windows_access_key),
        None => text.to_string(),
    };
    #[cfg(target_os = "macos")]
    let accelerator = key.map(|key| key.macos_accelerator);
    #[cfg(not(target_os = "macos"))]
    let accelerator = {
        let _ = key;
        None::<&str>
    };

    MenuItem::with_id(app, id, text, true, accelerator).map_err(|err| err.to_string())
}

/// Mark `key` as the access key of `text`: its first occurrence is
/// underlined when it appears in the text, as in English labels; otherwise
/// it is added after the text in parentheses, the Windows convention for
/// Chinese labels.
#[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
fn with_access_key(text: &str, key: char) -> String {
    let escaped = text.replace('&', "&&");
    match escaped
        .char_indices()
        .find(|(_, c)| c.eq_ignore_ascii_case(&key))
    {
        Some((index, _)) => format!("{}&{}", &escaped[..index], &escaped[index..]),
        None => format!("{escaped}(&{key})"),
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
        COPY_LAST_DICTATION => copy_last_dictation(),
        SHOW_MAIN_WINDOW => show_main_window(app),
        UPDATE_AVAILABLE => {
            show_main_window(app);
            // The main window switches to the About page, where the update
            // is installed.
            if let Err(err) = app.emit_to("main", "update:open", ()) {
                log::warn!("Failed to open the update page: {err}");
            }
        }
        QUIT_APP => app.exit(0),
        _ => {}
    }
}

/// Recovers a dictation whose paste did not arrive without opening History.
fn copy_last_dictation() {
    let text = crate::storage::get_or_create_device_id()
        .and_then(|device_id| crate::storage::latest_dictation_text(&device_id));
    match text {
        Ok(Some(text)) => {
            match arboard::Clipboard::new().and_then(|mut clipboard| clipboard.set_text(&text)) {
                Ok(()) => log::info!(
                    "Copied the last dictation to the clipboard (chars={})",
                    text.chars().count()
                ),
                Err(err) => log::warn!("Failed to copy the last dictation: {err}"),
            }
        }
        Ok(None) => log::info!("Copy last dictation: there is no dictation to copy"),
        Err(err) => log::warn!("Failed to read the last dictation: {err}"),
    }
}

/// Starting and stopping a read block (the system voice's stop waits on the
/// main thread), so they are handed off the way the hotkey hands them to its
/// worker.
fn run_off_main_thread(app: &AppHandle, action: impl FnOnce(&TtsController) + Send + 'static) {
    let tts = app.state::<TtsController>().inner().clone();
    tauri::async_runtime::spawn_blocking(move || action(&tts));
}

pub(crate) fn show_main_window(app: &AppHandle) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_key_underlines_its_first_occurrence() {
        assert_eq!(with_access_key("Read Translation", 'T'), "Read &Translation");
        assert_eq!(with_access_key("Open VoiceX", 'O'), "&Open VoiceX");
    }

    #[test]
    fn access_key_missing_from_the_text_is_appended() {
        assert_eq!(with_access_key("朗读原文", 'R'), "朗读原文(&R)");
    }

    #[test]
    fn ampersands_in_the_text_stay_literal() {
        assert_eq!(with_access_key("Q&A", 'X'), "Q&&A(&X)");
    }

    #[test]
    fn access_keys_are_unique() {
        let ids = [
            READ_SELECTION,
            TRANSLATE_SELECTION,
            STOP_READING,
            COPY_LAST_DICTATION,
            SHOW_MAIN_WINDOW,
            QUIT_APP,
            UPDATE_AVAILABLE,
        ];
        let mut keys: Vec<char> = ids
            .iter()
            .map(|id| item_key(id).expect("every item has a key").windows_access_key)
            .collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), ids.len());
    }
}
