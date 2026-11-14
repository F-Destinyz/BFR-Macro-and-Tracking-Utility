fn main() {
    let attributes =
        tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(&[
            "recording_bridge_ready",
            "record_click",
            "report_recording_error",
            "tracker_diagnostic_event",
            "tracker_dungeon_context",
            "tracker_reward_event",
            "tracker_reward_sample_event",
            "tracker_open_reward_samples_folder",
            "tracker_read_data_page",
            "tracker_clear_history",
            "tracker_delete_history_entry",
            "tracker_get_pinned",
            "tracker_set_pinned",
        ]));
    tauri_build::try_build(attributes).expect("failed to build Tauri application metadata");
}
