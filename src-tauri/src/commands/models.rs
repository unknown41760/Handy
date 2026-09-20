use crate::managers::model::{ModelInfo, ModelManager};
use crate::managers::transcription::{ModelStateEvent, TranscriptionManager};
use crate::settings::{
    get_settings, reconcile_preset_model_capabilities, write_settings, AppSettings,
    ModelUnloadTimeout,
};
use log::{error, warn};
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

fn reconcile_use_current_presets_for_model(
    settings: &mut AppSettings,
    supported_languages: &[String],
    supports_language_detection: bool,
    supports_translation: bool,
) -> bool {
    let mut changed = false;
    for preset in settings
        .transcription_presets
        .iter_mut()
        .filter(|preset| preset.model_id.trim().is_empty())
    {
        changed |= reconcile_preset_model_capabilities(
            preset,
            supported_languages,
            supports_language_detection,
            supports_translation,
        );
    }
    changed
}

pub fn reconcile_preset_capabilities_on_startup(app: &AppHandle) -> bool {
    let model_manager = app.state::<Arc<ModelManager>>();
    let mut settings = get_settings(app);
    let selected_model = settings.selected_model.clone();
    let mut changed = false;

    for preset in &mut settings.transcription_presets {
        let effective_model_id = if preset.model_id.trim().is_empty() {
            selected_model.as_str()
        } else {
            preset.model_id.as_str()
        };
        if effective_model_id.is_empty() {
            continue;
        }
        if let Some(model) = model_manager.get_model_info(effective_model_id) {
            changed |= reconcile_preset_model_capabilities(
                preset,
                &model.supported_languages,
                model.supports_language_detection,
                model.supports_translation,
            );
        }
    }

    if changed {
        write_settings(app, settings);
        let _ = app.emit(
            "settings-changed",
            serde_json::json!({ "setting": "transcription_presets" }),
        );
    }

    changed
}

fn restore_preset_shortcuts(
    app: &AppHandle,
    bindings: &[crate::settings::ShortcutBinding],
    fallback_bindings: &[crate::settings::ShortcutBinding],
) -> Result<(), String> {
    let mut failures = Vec::new();
    for binding in bindings {
        if let Err(error) = crate::shortcut::register_shortcut(app, binding.clone()) {
            failures.push(format!("{}: {}", binding.id, error));
        }
    }

    let fallback_restore_error =
        crate::secure_input::restore_suspended_binding_fallback(app, fallback_bindings).err();

    // A failed targeted restore marks the missing shadow as uncovered, so a
    // checked reconciliation gets one immediate retry and keeps status truthful.
    match crate::secure_input::reconcile_fallback_checked(app) {
        Ok(()) => {
            if let Some(error) = fallback_restore_error {
                warn!(
                    "Secure Input fallback restore initially failed but reconciliation recovered it: {}",
                    error
                );
            }
        }
        Err(error) => {
            if let Some(restore_error) = fallback_restore_error {
                failures.push(format!(
                    "Secure Input fallback restore: {}; reconcile: {}",
                    restore_error, error
                ));
            } else {
                failures.push(format!("Secure Input fallback reconcile: {}", error));
            }
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

fn model_delete_error_with_rollback(
    app: &AppHandle,
    bindings: &[crate::settings::ShortcutBinding],
    fallback_bindings: &[crate::settings::ShortcutBinding],
    primary: String,
) -> String {
    match restore_preset_shortcuts(app, bindings, fallback_bindings) {
        Ok(()) => primary,
        Err(restore_error) => format!(
            "{}; preset-shortcut rollback incomplete: {}",
            primary, restore_error
        ),
    }
}

#[tauri::command]
#[specta::specta]
pub async fn delete_model(
    app_handle: AppHandle,
    model_manager: State<'_, Arc<ModelManager>>,
    transcription_manager: State<'_, Arc<TranscriptionManager>>,
    model_id: String,
) -> Result<(), String> {
    if crate::settings::has_active_transcription_operation(&app_handle) {
        return Err("Cannot delete a model while a transcription recording is active".to_string());
    }
    if crate::settings::is_transcription_model_processing(&app_handle, &model_id) {
        return Err(
            "Cannot delete this model while a transcription operation is still processing"
                .to_string(),
        );
    }

    let settings_before = get_settings(&app_handle);
    let deleting_selected_model = settings_before.selected_model == model_id;
    let deleting_loaded_model =
        transcription_manager.get_current_model().as_deref() == Some(model_id.as_str());

    let mut next_settings = settings_before.clone();
    if deleting_selected_model {
        next_settings.selected_model = String::new();
    }
    let disabled_preset_ids =
        reconcile_presets_for_deleted_model(&mut next_settings, &model_id, deleting_selected_model);
    let bindings_to_unregister = disabled_preset_ids
        .iter()
        .filter_map(|id| settings_before.bindings.get(id).cloned())
        .collect::<Vec<_>>();

    let mut unregistered = Vec::new();
    let mut suspended_fallback = Vec::new();
    for binding in &bindings_to_unregister {
        let fallback = match crate::secure_input::suspend_binding_fallback(&app_handle, &binding.id)
        {
            Ok(fallback) => fallback,
            Err(error) => {
                return Err(model_delete_error_with_rollback(
                    &app_handle,
                    &unregistered,
                    &suspended_fallback,
                    format!(
                        "Failed to suspend Secure Input fallback for preset '{}' before deleting model: {}",
                        binding.id, error
                    ),
                ));
            }
        };
        suspended_fallback.extend(fallback);

        if let Err(error) = crate::shortcut::unregister_shortcut(&app_handle, binding.clone()) {
            return Err(model_delete_error_with_rollback(
                &app_handle,
                &unregistered,
                &suspended_fallback,
                format!(
                    "Failed to unregister preset shortcut '{}' before deleting model: {}",
                    binding.id, error
                ),
            ));
        }
        unregistered.push(binding.clone());
    }

    // A shortcut may have fired after the initial guard but before its primary
    // and Carbon registrations were removed. Re-check after teardown and abort
    // before touching the model if an operation won that race.
    if crate::settings::has_active_transcription_operation(&app_handle)
        || crate::settings::is_transcription_model_processing(&app_handle, &model_id)
    {
        return Err(model_delete_error_with_rollback(
            &app_handle,
            &unregistered,
            &suspended_fallback,
            "Cannot delete the model because a transcription operation started while deletion was being prepared"
                .to_string(),
        ));
    }

    if deleting_loaded_model {
        if let Err(error) = transcription_manager.unload_model() {
            return Err(model_delete_error_with_rollback(
                &app_handle,
                &unregistered,
                &suspended_fallback,
                format!("Failed to unload model: {}", error),
            ));
        }
    }

    if let Err(error) = model_manager.delete_model(&model_id) {
        return Err(model_delete_error_with_rollback(
            &app_handle,
            &unregistered,
            &suspended_fallback,
            error.to_string(),
        ));
    }

    write_settings(&app_handle, next_settings);
    let _ = app_handle.emit(
        "settings-changed",
        serde_json::json!({ "setting": "transcription_presets" }),
    );
    if let Err(error) = crate::secure_input::reconcile_fallback_checked(&app_handle) {
        warn!(
            "Failed to reconcile Secure Input fallback after model deletion: {}",
            error
        );
    }

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

    let settings_before = get_settings(app);
    let unload_timeout = settings_before.model_unload_timeout;

    // Persist the new selection early so the frontend sees the correct model
    // when it reacts to events emitted by load_model. Presets that inherit the
    // current model are normalized in the same write. Keep the exact previous
    // settings so a failed load can also roll back those preset normalizations.
    let mut settings = settings_before.clone();
    settings.selected_model = model_id.to_string();
    settings.onboarding_completed = true;
    reconcile_use_current_presets_for_model(
        &mut settings,
        &model_info.supported_languages,
        model_info.supports_language_detection,
        model_info.supports_translation,
    );

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
        write_settings(app, settings_before);
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
    use super::{reconcile_presets_for_deleted_model, reconcile_use_current_presets_for_model};
    use crate::settings::{get_default_settings, TranscriptionPreset};

    fn test_preset(id: &str, model_id: &str) -> TranscriptionPreset {
        TranscriptionPreset {
            id: id.to_string(),
            name: id.to_string(),
            enabled: true,
            model_id: model_id.to_string(),
            language: "auto".to_string(),
            translate_to_english: false,
            post_process: false,
            post_process_prompt_id: None,
        }
    }

    #[test]
    fn current_model_switch_reconciles_inherited_preset_capabilities() {
        let mut settings = get_default_settings();
        let mut inherited = test_preset("preset_inherited", "");
        inherited.language = "auto".to_string();
        inherited.translate_to_english = true;
        let mut explicit = test_preset("preset_explicit", "other-model");
        explicit.language = "auto".to_string();
        explicit.translate_to_english = true;
        settings.transcription_presets = vec![inherited, explicit];

        assert!(reconcile_use_current_presets_for_model(
            &mut settings,
            &["en-US".to_string(), "nl-NL".to_string()],
            false,
            false,
        ));

        assert_eq!(settings.transcription_presets[0].language, "en");
        assert!(!settings.transcription_presets[0].translate_to_english);
        assert_eq!(settings.transcription_presets[1].language, "auto");
        assert!(settings.transcription_presets[1].translate_to_english);
    }

    #[test]
    fn deleting_explicit_preset_model_disables_and_resets_that_preset() {
        let mut settings = get_default_settings();
        settings.selected_model = "normal-model".to_string();
        settings.transcription_presets = vec![
            test_preset("preset_a", "preset-model"),
            test_preset("preset_b", "other-model"),
        ];

        let disabled = reconcile_presets_for_deleted_model(&mut settings, "preset-model", false);

        assert_eq!(disabled, vec!["preset_a".to_string()]);
        assert!(!settings.transcription_presets[0].enabled);
        assert!(settings.transcription_presets[0].model_id.is_empty());
        assert!(settings.transcription_presets[1].enabled);
        assert_eq!(settings.transcription_presets[1].model_id, "other-model");
    }

    #[test]
    fn deleting_current_model_disables_use_current_presets() {
        let mut settings = get_default_settings();
        settings.selected_model = "normal-model".to_string();
        settings.transcription_presets = vec![
            test_preset("preset_a", ""),
            test_preset("preset_b", "other-model"),
        ];

        let disabled = reconcile_presets_for_deleted_model(&mut settings, "normal-model", true);

        assert_eq!(disabled, vec!["preset_a".to_string()]);
        assert!(!settings.transcription_presets[0].enabled);
        assert!(settings.transcription_presets[1].enabled);
    }
}
