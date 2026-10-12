//! Savvy Tauri app. The webview is the UI; this binary exists to give
//! it the bits the webview itself cannot do: persistent storage, native
//! media keys, the loopback HTTP proxy that hides googlevideo origin
//! checks from the embedded player, and the InnerTube client.

pub mod audio;
mod commands;
mod db;
mod newpipe;
pub mod mux;
pub mod player_bridge;
pub mod proxy;
mod sponsorblock;
mod state;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub async fn build_app_state() -> state::AppState {
    state::AppState::new().await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,savvy=debug")),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .setup(|app| {
            // Session-only media cache: sweep anything the previous session
            // left behind (a crash, or files ffmpeg still held open at exit).
            state::AppState::clear_transient_caches();
            let app_state = tauri::async_runtime::block_on(state::AppState::new());
            // Created eagerly but lazily *used*: the offscreen window is only
            // opened on the first fallback resolve, so a session that never hits
            // a block never loads youtube.com.
            let _ = app_state.bridge.set(player_bridge::PlayerBridge::new(
                app.handle().clone(),
                app_state.bridge_inbox.clone(),
                state::bridge_report_url(app_state.proxy_addr),
            ));
            app.manage(app_state);
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::ping,
            commands::bootstrap,
            commands::innertube_reset,
            commands::search,
            commands::search_all,
            commands::search_channels,
            commands::search_suggestions,
            commands::youtube_playlist,
            commands::video_info,
            commands::video_resolve_streams,
            commands::audio_source,
            commands::video_mux_source,
            commands::history_list,
            commands::history_clear,
            commands::history_remove,
            commands::history_add,
            commands::history_get,
            commands::history_progress,
            commands::playlists_list,
            commands::playlist_create,
            commands::playlist_delete,
            commands::playlist_add_item,
            commands::playlist_remove_item,
            commands::playlist_items,
            commands::saved_add,
            commands::saved_list,
            commands::saved_ids,
            commands::saved_remove,
            commands::setting_set,
            commands::setting_get,
            commands::settings_all,
            commands::setting_reset,
            commands::subscription_add,
            commands::subscription_remove,
            commands::subscription_list,
            commands::subscription_exists,
            commands::subscription_set_categories,
            commands::category_list,
            commands::category_add,
            commands::category_rename,
            commands::category_remove,
            commands::subscription_feed,
            commands::subscription_import,
            commands::subscription_import_file,
            commands::subscription_refresh_avatars,
            commands::channel_get_info,
            commands::channel_get_videos,
            commands::related_videos,
            commands::sponsor_segments,
            commands::captions_list,
            commands::captions_fetch,
            commands::video_chapters,
            commands::open_external,
        ])
        .build(tauri::generate_context!())
        .expect("error while building savvy")
        .run(|_app, event| match event {
            // Free the per-session playback caches when the app exits. Anything
            // an ffmpeg process still holds open at this point, or a hard kill
            // that skips this entirely, is covered by the startup sweep.
            tauri::RunEvent::Exit => state::AppState::clear_transient_caches(),
            _ => {}
        });
}
