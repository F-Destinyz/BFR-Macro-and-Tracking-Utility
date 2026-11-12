fn main() {
    let attributes = tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "recording_bridge_ready",
            "record_click",
            "report_recording_error",
            "tracker_diagnostic_event",
            "tracker_reward_event",
            "tracker_read_data",
        ]),
    );
    tauri_build::try_build(attributes).expect("failed to build Tauri application metadata");
}
