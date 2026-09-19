use crate::managers::model::{ModelInfo, ModelManager};
use crate::managers::transcription::{ModelStateEvent, TranscriptionManager};
use crate::settings::{get_settings, write_settings, AppSettings, ModelUnloadTimeout};
use log::error;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};

#[tauri::command]
#[specta::specta]
pub async fn get_available_models(
    model_manager: State<'_, Arc<ModelManager>>,
) -> Result<Vec<ModelInfo>, String> {
    Ok(model_manager.get_available_models())
}

#[tauri::command]
#[specta::specta]
pub async fn get_model_info(
    model_manager: State<'_, Arc<ModelManager>>,
    model_id: String,
) -> Result<Option<ModelInfo>, String> {
    Ok(model_manager.get_model_info(&model_id))
}

/// Re-scan local sources (custom models dir + shared HF cache) for models added
/// since launch
#[tauri::command]
#[specta::specta]
pub async fn rescan_local_models(
    model_manager: State<'_, Arc<ModelManager>>,
) -> Result<(), String> {
    let mm = model_manager.inner().clone();
    tokio::task::spawn_blocking(move || mm.rescan_local_models())
        .await
        .map_err(|e| format!("rescan task panicked: {e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn download_model(
    app_handle: AppHandle,
    model_manager: State<'_, Arc<ModelManager>>,
    model_id: String,
) -> Result<(), String> {
    let result = model_manager
        .download_model(&model_id)
        .await
        .map_err(|e| e.to_string());

    if let Err(ref error) = result {
        // Log as well as emit: the toast is transient, and failed downloads have
        // historically been undiagnosable because logs showed nothing (#1579).
        error!("Model download failed for {}: {}", model_id, error);
        let _ = app_handle.emit(
            "model-download-failed",
            serde_json::json!({ "model_id": &model_id, "error": error }),
        );
    }

    result
}

fn reconcile_presets_for_deleted_model(
    settings: &mut AppSettings,
    model_id: &str,
    deleting_selected_model: bool,
) -> Vec<String> {
    let mut disabled = Vec::new();
    for preset in &mut settings.transcription_presets {
        let explicitly_references_deleted = preset.model_id == model_id;
        let inherited_deleted_selection =
            deleting_selected_model && preset.model_id.trim().is_empty();
        if explicitly_references_deleted || inherited_deleted_selection {
            if preset.enabled {
                disabled.push(preset.id.clone());
            }
            preset.enabled = false;
            if explicitly_references_deleted {
                preset.model_id.clear();
            }
        }
    }
    disabled
}

#[tauri::command]
#[specta::specta]
pub async fn delete_model(
    app_handle: AppHandle,
    model_manager: State<'_, Arc<ModelManager>>,
    transcription_manager: State<'_, Arc<TranscriptionManager>>,
    model_id: String,
) -> Result<(), String> {
    let settings_before = get_settings(&app_handle);
    let deleting_selected_model = settings_before.selected_model == model_id;
    let deleting_loaded_model =
        transcription_manager.get_current_model().as_deref() == Some(model_id.as_str());

    // A loaded model must be released before its files can be removed. A preset
    // can leave a different model resident than `selected_model`, so check the
    // actual loaded engine rather than only the persistent selection. Delay all
    // persisted settings changes until deletion succeeds.
    if deleting_loaded_model {
        transcription_manager
            .unload_model()
            .map_err(|e| format!("Failed to unload model: {}", e))?;
    }

    model_manager
        .delete_model(&model_id)
        .map_err(|e| e.to_string())?;

    let mut settings = get_settings(&app_handle);
    if deleting_selected_model {
        settings.selected_model = String::new();
    }

    // Disable every enabled preset whose effective model was just deleted. An
    // explicit reference is also reset to "Use current model" so the stale id
    // cannot survive in persisted settings.
    let disabled_preset_ids =
        reconcile_presets_for_deleted_model(&mut settings, &model_id, deleting_selected_model);
    let bindings_to_unregister: Vec<_> = disabled_preset_ids
        .iter()
        .filter_map(|id| settings.bindings.get(id).cloned())
        .collect();

    // First try to release any OS registrations while the preset transition is
    // still in-flight. Persist the disabled state regardless: if a backend
    // unregistration fails, broad shortcut cleanup intentionally retries all
    // preset bindings (including disabled ones) on the next cleanup/switch.
    for binding in bindings_to_unregister {
        if let Err(error) = crate::shortcut::unregister_shortcut(&app_handle, binding) {
            log::warn!("Failed to unregister preset for deleted model: {}", error);
        }
    }

    write_settings(&app_handle, settings);
    let _ = app_handle.emit(
        "settings-changed",
        serde_json::json!({ "setting": "transcription_presets" }),
    );
    crate::secure_input::reconcile_fallback(&app_handle);

    Ok(())
}

/// Shared logic for switching the active model, used by both the Tauri command
/// and the tray menu handler.
///
/// Validates the model, updates the persisted setting, and loads the model
/// unless the unload timeout is set to "Immediately" (in which case the model
/// will be loaded on-demand during the next transcription).
pub fn switch_active_model(app: &AppHandle, model_id: &str) -> Result<(), String> {
    let model_manager = app.state::<Arc<ModelManager>>();
    let transcription_manager = app.state::<Arc<TranscriptionManager>>();

    // Atomically claim the loading slot — prevents concurrent model loads
    // from tray double-clicks or overlapping commands. The guard resets the
    // flag on drop (including early returns, errors, and panics).
    let _loading_guard = transcription_manager
        .try_start_loading()
        .ok_or_else(|| "Model load already in progress".to_string())?;

    // Check if model exists and is available
    let model_info = model_manager
        .get_model_info(model_id)
        .ok_or_else(|| format!("Model not found: {}", model_id))?;

    if !model_info.is_downloaded {
        return Err(format!("Model not downloaded: {}", model_id));
    }

    let settings = get_settings(app);
    let unload_timeout = settings.model_unload_timeout;
    let old_model = settings.selected_model.clone();
    let old_onboarding_completed = settings.onboarding_completed;

    // Persist the new selection early so the frontend sees the correct model
    // when it reacts to events emitted by load_model.
    let mut settings = settings;
    settings.selected_model = model_id.to_string();
    settings.onboarding_completed = true;

    write_settings(app, settings);

    // Skip eager loading if unload is set to "Immediately" — the model
    // will be loaded on-demand during the next transcription.
    if unload_timeout == ModelUnloadTimeout::Immediately {
        // Notify frontend — load_model won't be called so no events
        // would otherwise be emitted.
        let _ = app.emit(
            "model-state-changed",
            ModelStateEvent {
                event_type: "selection_changed".to_string(),
                model_id: Some(model_id.to_string()),
                model_name: Some(model_info.name.clone()),
                error: None,
            },
        );
        log::info!(
            "Model selection changed to {} (not loading — unload set to Immediately).",
            model_id
        );
        return Ok(());
    }

    // Load the model. On failure, revert the persisted selection.
    if let Err(e) = transcription_manager.load_model(model_id) {
        let mut settings = get_settings(app);
        settings.selected_model = old_model;
        settings.onboarding_completed = old_onboarding_completed;
        write_settings(app, settings);
        return Err(e.to_string());
    }

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn set_active_model(
    app_handle: AppHandle,
    _model_manager: State<'_, Arc<ModelManager>>,
    _transcription_manager: State<'_, Arc<TranscriptionManager>>,
    model_id: String,
) -> Result<(), String> {
    switch_active_model(&app_handle, &model_id)
}

#[tauri::command]
#[specta::specta]
pub async fn get_current_model(app_handle: AppHandle) -> Result<String, String> {
    let settings = get_settings(&app_handle);
    Ok(settings.selected_model)
}

#[tauri::command]
#[specta::specta]
pub async fn get_transcription_model_status(
    transcription_manager: State<'_, Arc<TranscriptionManager>>,
) -> Result<Option<String>, String> {
    Ok(transcription_manager.get_current_model())
}

#[tauri::command]
#[specta::specta]
pub async fn is_model_loading(
    transcription_manager: State<'_, Arc<TranscriptionManager>>,
) -> Result<bool, String> {
    // Check if transcription manager has a loaded model
    let current_model = transcription_manager.get_current_model();
    Ok(current_model.is_none())
}

#[tauri::command]
#[specta::specta]
pub async fn cancel_download(
    model_manager: State<'_, Arc<ModelManager>>,
    model_id: String,
) -> Result<(), String> {
    model_manager
        .cancel_download(&model_id)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::reconcile_presets_for_deleted_model;
    use crate::settings::get_default_settings;

    #[test]
    fn deleting_explicit_preset_model_disables_and_resets_that_preset() {
        let mut settings = get_default_settings();
        settings.selected_model = "normal-model".to_string();
        settings.transcription_presets[0].enabled = true;
        settings.transcription_presets[0].model_id = "preset-model".to_string();
        settings.transcription_presets[1].enabled = true;
        settings.transcription_presets[1].model_id = "other-model".to_string();

        let disabled = reconcile_presets_for_deleted_model(&mut settings, "preset-model", false);

        assert_eq!(disabled, vec!["preset_1".to_string()]);
        assert!(!settings.transcription_presets[0].enabled);
        assert!(settings.transcription_presets[0].model_id.is_empty());
        assert!(settings.transcription_presets[1].enabled);
        assert_eq!(settings.transcription_presets[1].model_id, "other-model");
    }

    #[test]
    fn deleting_current_model_disables_use_current_presets() {
        let mut settings = get_default_settings();
        settings.selected_model = "normal-model".to_string();
        settings.transcription_presets[0].enabled = true;
        settings.transcription_presets[0].model_id.clear();
        settings.transcription_presets[1].enabled = true;
        settings.transcription_presets[1].model_id = "other-model".to_string();

        let disabled = reconcile_presets_for_deleted_model(&mut settings, "normal-model", true);

        assert_eq!(disabled, vec!["preset_1".to_string()]);
        assert!(!settings.transcription_presets[0].enabled);
        assert!(settings.transcription_presets[1].enabled);
    }
}
