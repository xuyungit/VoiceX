//! VoiceX - Cross-platform voice input application
//!
//! This is the main library module that exports all functionality to Tauri.

pub mod asr;
pub mod audio;
pub mod commands;
pub mod foreground_app;
pub mod hotkey;
pub mod hud;
pub mod i18n;
pub mod injector;
pub mod llm;
pub mod services;
pub mod session;
pub mod state;
pub mod storage;
pub mod ui_locale;

use crate::audio::AudioService;
use crate::commands::settings::AppSettings;
use crate::services::{asr_debug_service::AsrDebugService, sync_service::SyncService};
use hotkey::{HotkeyConfiguration, HotkeyManager};
use session::{SessionController, SessionCoordinator};
use tauri::Manager;

#[cfg(target_os = "macos")]
fn enforce_macos_release_install_path() -> Result<(), Box<dyn std::error::Error>> {
    // Keep debug/dev builds flexible; only enforce stable path for release builds.
    if cfg!(debug_assertions) {
        return Ok(());
    }

    let executable_path = std::env::current_exe()?;
    let expected_app_path = std::path::Path::new("/Applications/VoiceX.app");
    if executable_path.starts_with(expected_app_path) {
        return Ok(());
    }

    let message = format!(
        "Release builds must run from /Applications/VoiceX.app to keep macOS permissions stable. Current executable: {}",
        executable_path.display()
    );
    Err(std::io::Error::new(std::io::ErrorKind::Other, message).into())
}

#[cfg(target_os = "windows")]
fn apply_windows_tray_icon(app: &tauri::App) {
    let Some(tray) = app.tray_by_id("main") else {
        log::warn!("Tray icon 'main' not found; Windows tray icon not updated");
        return;
    };

    let tray_icon =
        match tauri::image::Image::from_bytes(include_bytes!("../icons/trayWindows.png")) {
            Ok(icon) => icon,
            Err(err) => {
                log::warn!("Failed to load Windows tray icon asset: {}", err);
                return;
            }
        };

    if let Err(err) = tray.set_icon(Some(tray_icon)) {
        log::warn!("Failed to apply Windows tray icon: {}", err);
    }
}

/// Initialize the application state and services
pub fn init_app(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logger (default to info if RUST_LOG not set)
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();
    log::info!("VoiceX starting...");

    // Initialize database
    let app_data_dir = app.path().app_data_dir()?;
    std::fs::create_dir_all(&app_data_dir)?;

    let db_path = app_data_dir.join("voicex.db");
    log::info!("Database path: {:?}", db_path);

    // Initialize storage
    storage::init_database(&db_path)?;

    // Initialize audio recordings directory
    let recordings_dir = app_data_dir.join("recordings");
    let audio_service: tauri::State<'_, AudioService> = app.state();
    audio_service.init_paths(recordings_dir)?;

    // Initialize hotkey listener & session controller
    let manager: tauri::State<'_, HotkeyManager> = app.state();
    let session: tauri::State<'_, SessionController> = app.state();
    let session_controller = session.inner().clone();
    session_controller.init_with_handle(&app.handle());

    let persisted_settings = match storage::get_settings() {
        Ok(settings) => settings,
        Err(e) => {
            log::warn!("Failed to load settings, falling back to defaults: {}", e);
            AppSettings::default()
        }
    };

    let sync_service: tauri::State<'_, SyncService> = app.state();
    sync_service.init_with_handle(app.handle());
    let asr_debug_service: tauri::State<'_, AsrDebugService> = app.state();
    asr_debug_service.install_global();
    if !persisted_settings.enable_diagnostics {
        asr_debug_service.clear_soniox_debug_overrides_now()?;
    }
    // Pre-create the HUD while hidden so the first hotkey press doesn't have to
    // pay the window creation cost or flash at the default window position.
    if let Err(err) = hud::create_hud_window(&app.handle()) {
        log::warn!("Failed to create HUD window: {}", err);
    }
    let session_coordinator = SessionCoordinator::new(session_controller.clone());
    session_controller.apply_settings(
        persisted_settings.hold_threshold_ms,
        persisted_settings.max_recording_minutes,
        &persisted_settings.text_injection_mode,
        persisted_settings.text_injection_overrides.clone(),
        persisted_settings.input_device_uid.clone(),
        persisted_settings.remove_trailing_punctuation,
        persisted_settings.short_sentence_threshold,
        persisted_settings.replacement_rules.clone(),
        persisted_settings.translation_enabled,
        &persisted_settings.translation_trigger_mode,
        persisted_settings.double_tap_window_ms,
    );
    sync_service.apply_settings(&persisted_settings);
    manager.start_listener(app.handle().clone(), Some(session_coordinator.clone()));
    #[cfg(target_os = "macos")]
    {
        let permission = hotkey::HotkeyPermissionStatus::detect();
        if !permission.accessibility || !permission.input_monitoring {
            log::warn!(
                "Missing hotkey permissions (accessibility={}, input_monitoring={})",
                permission.accessibility,
                permission.input_monitoring
            );
        } else {
            log::info!(
                "Hotkey permissions OK (accessibility={}, input_monitoring={})",
                permission.accessibility,
                permission.input_monitoring
            );
        }
    }
    // Apply persisted hotkey if present
    if let Some(cfg) = persisted_settings
        .hotkey_config
        .and_then(|s| HotkeyConfiguration::from_storage(&s))
    {
        manager.set_config(Some(cfg));
    } else {
        manager.set_config(Some(HotkeyConfiguration::default()));
    }

    log::info!("VoiceX initialized successfully");
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Prevent Tauri from capturing DeviceEvent on Windows, which would
        // interfere with rdev's low-level keyboard hook when the main window is focused.
        .device_event_filter(tauri::DeviceEventFilter::Always)
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(main) = app.get_webview_window("main") {
                let _ = main.show();
                let _ = main.unminimize();
                let _ = main.set_focus();

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
        }))
        .manage(audio::AudioService::default())
        .manage(AsrDebugService::default())
        .manage(HotkeyManager::default())
        .manage(SessionController::default())
        .manage(SyncService::default())
        .on_window_event(|window, event| {
            // Keep the process alive when the main window is closed; just hide it.
            if window.label() == "main" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }

            // Recompute HUD geometry when DPI scaling changes.
            if window.label() == "hud" {
                if let tauri::WindowEvent::ScaleFactorChanged { .. } = event {
                    if let Some(hud_window) = window.app_handle().get_webview_window("hud") {
                        if let Err(err) = hud::refresh_hud_window(&hud_window) {
                            log::warn!("Failed to refresh HUD geometry: {}", err);
                        }
                    }
                }
            }
        })
        .on_menu_event(|app, event| {
            if event.id() == "show_main_window" {
                if let Some(main) = app.get_webview_window("main") {
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
            } else if event.id() == "quit_app" {
                app.exit(0);
            }
        })
        .setup(|app| {
            #[cfg(target_os = "macos")]
            if let Err(err) = enforce_macos_release_install_path() {
                eprintln!("{}", err);
                return Err(err);
            }

            // Accessory policy: no Dock icon, no "home Space".
            // This lets the HUD window appear on whichever Space is active.
            // Main window is accessible via the tray icon menu.
            #[cfg(target_os = "macos")]
            {
                use objc2::MainThreadMarker;
                use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
                // setup() is always called on the main thread; use the safe check.
                let mtm = MainThreadMarker::new()
                    .expect("Tauri setup must run on the main thread");
                let ns_app = NSApplication::sharedApplication(mtm);
                ns_app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
                log::info!("macOS activation policy set to Accessory");
            }

            if let Err(e) = init_app(app) {
                log::error!("Failed to initialize app: {}", e);
            }
            #[cfg(target_os = "windows")]
            apply_windows_tray_icon(app);
            #[cfg(desktop)]
            {
                let preferred_language = storage::get_settings()
                    .map(|settings| settings.ui_language)
                    .unwrap_or_else(|_| ui_locale::UI_LANGUAGE_SYSTEM.to_string());

                if let Err(err) = i18n::apply_tray_menu(&app.handle(), &preferred_language) {
                    log::warn!("Failed to attach tray menu: {}", err);
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::audio::get_input_devices,
            commands::audio::set_input_device,
            commands::audio::start_audio_capture,
            commands::audio::stop_audio_capture,
            commands::audio::get_recordings_dir,
            commands::audio::open_recordings_dir,
            commands::build_info::get_build_info,
            commands::hud::set_hud_content_bounds,
            commands::hud::hud_ready,
            commands::hotkey::record_hotkey,
            commands::hotkey::apply_hotkey_config,
            commands::hotkey::current_hotkey,
            commands::hotkey::hotkey_permission_status,
            commands::settings::get_settings,
            commands::settings::get_recent_target_apps,
            commands::settings::get_resolved_ui_locale,
            commands::settings::save_settings,
            commands::settings::probe_local_asr,
            commands::settings::probe_current_asr_provider,
            commands::settings::load_provider_probe_audio,
            commands::settings::get_soniox_debug_harness_status,
            commands::settings::start_soniox_debug_mock_server,
            commands::settings::stop_soniox_debug_mock_server,
            commands::settings::set_soniox_debug_fault_mode,
            commands::settings::clear_soniox_debug_overrides,
            commands::history::get_history,
            commands::history::delete_history_record,
            commands::history::get_usage_stats,
            commands::history::get_local_usage_stats,
            commands::history::load_audio_file,
            commands::sync::get_sync_state,
            commands::sync::sync_now,
            commands::hotword::sync_hotwords,
            commands::hotword::force_download_hotwords,
            commands::hotword::list_online_vocabularies,
            commands::retranscribe::re_transcribe,
            commands::retranscribe::cancel_retranscribe,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
