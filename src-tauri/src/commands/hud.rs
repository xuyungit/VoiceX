//! HUD-related commands

use crate::session::SessionController;

#[tauri::command]
pub fn hud_ready(session: tauri::State<'_, SessionController>) {
    session.replay_hud_snapshot();
}
