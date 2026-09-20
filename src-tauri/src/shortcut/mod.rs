//! Keyboard shortcut management module
//!
//! This module provides a unified interface for keyboard shortcuts with
//! multiple backend implementations:
//!
//! - `tauri`: Uses Tauri's built-in global-shortcut plugin
//! - `handy_keys`: Uses the handy-keys library for more control
//!
//! The active implementation is determined by the `keyboard_implementation`
//! setting and can be changed at runtime.

mod handler;
pub mod handy_keys;
pub mod tauri_impl;

use log::{debug, error, info, warn};
use serde::Serialize;
use specta::Type;
use tauri::{AppHandle, Emitter, Manager};
use uuid::Uuid;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use crate::settings::APPLE_INTELLIGENCE_DEFAULT_MODEL_ID;
use crate::settings::{
    self, get_settings, AutoSubmitKey, ClipboardHandling, KeyboardImplementation, LLMPrompt,
    OverlayPosition, OverlayStyle, PasteMethod, ShortcutActivation, ShortcutBinding, SoundTheme,
    Theme, TranscriptionPreset, TypingTool, VadBackend, APPLE_INTELLIGENCE_PROVIDER_ID,
};
use crate::tray;

// Note: Commands are accessed via shortcut::handy_keys:: in lib.rs

/// Initialize shortcuts using the configured implementation
pub fn init_shortcuts(app: &AppHandle) {
    let user_settings = settings::load_or_create_app_settings(app);

    // Check which implementation to use
    match user_settings.keyboard_implementation {
        KeyboardImplementation::Tauri => {
            tauri_impl::init_shortcuts(app);
        }
        KeyboardImplementation::HandyKeys => {
            if let Err(e) = handy_keys::init_shortcuts(app) {
                error!("Failed to initialize handy-keys shortcuts: {}", e);
                // Fall back to Tauri implementation and persist this fallback
                warn!("Falling back to Tauri global shortcut implementation and saving fallback to settings");

                // Update settings to persist the fallback so we don't retry HandyKeys on next launch
                let mut settings = settings::get_settings(app);
                settings.keyboard_implementation = KeyboardImplementation::Tauri;
                settings::write_settings(app, settings);

                tauri_impl::init_shortcuts(app);
            }
        }
    }
}

/// Register the cancel shortcut (called when recording starts)
pub fn register_cancel_shortcut(app: &AppHandle) {
    // Track recording lifecycle independently of the current implementation so
    // switching implementations mid-recording cannot leave stale fallback state.
    crate::secure_input::register_cancel_fallback(app);

    let settings = get_settings(app);
    match settings.keyboard_implementation {
        KeyboardImplementation::Tauri => tauri_impl::register_cancel_shortcut(app),
        KeyboardImplementation::HandyKeys => handy_keys::register_cancel_shortcut(app),
    }
}

/// Unregister the cancel shortcut (called when recording stops)
pub fn unregister_cancel_shortcut(app: &AppHandle) {
    crate::secure_input::unregister_cancel_fallback(app);

    let settings = get_settings(app);
    match settings.keyboard_implementation {
        KeyboardImplementation::Tauri => tauri_impl::unregister_cancel_shortcut(app),
        KeyboardImplementation::HandyKeys => handy_keys::unregister_cancel_shortcut(app),
    }
}

/// Register a shortcut using the appropriate implementation
pub fn register_shortcut(app: &AppHandle, binding: ShortcutBinding) -> Result<(), String> {
    let settings = get_settings(app);
    match settings.keyboard_implementation {
        KeyboardImplementation::Tauri => tauri_impl::register_shortcut(app, binding),
        KeyboardImplementation::HandyKeys => handy_keys::register_shortcut(app, binding),
    }
}

/// Unregister a shortcut using the appropriate implementation
pub fn unregister_shortcut(app: &AppHandle, binding: ShortcutBinding) -> Result<(), String> {
    let settings = get_settings(app);
    match settings.keyboard_implementation {
        KeyboardImplementation::Tauri => tauri_impl::unregister_shortcut(app, binding),
        KeyboardImplementation::HandyKeys => handy_keys::unregister_shortcut(app, binding),
    }
}

// ============================================================================
// Binding Management Commands
// ============================================================================

#[derive(Serialize, Type)]
pub struct BindingResponse {
    success: bool,
    binding: Option<ShortcutBinding>,
    error: Option<String>,
}

fn shortcuts_equivalent_for_implementation(
    left: &str,
    right: &str,
    implementation: KeyboardImplementation,
) -> bool {
    match implementation {
        KeyboardImplementation::Tauri => {
            let left = left.parse::<tauri_plugin_global_shortcut::Shortcut>();
            let right = right.parse::<tauri_plugin_global_shortcut::Shortcut>();
            matches!((left, right), (Ok(left), Ok(right)) if left == right)
        }
        KeyboardImplementation::HandyKeys => {
            let left = left.parse::<::handy_keys::Hotkey>();
            let right = right.parse::<::handy_keys::Hotkey>();
            matches!((left, right), (Ok(left), Ok(right)) if left == right)
        }
    }
}

fn validate_binding_conflict(
    app_settings: &settings::AppSettings,
    id: &str,
    binding: &str,
) -> Result<(), String> {
    for (other_id, other_binding) in &app_settings.bindings {
        if other_id == id || !settings::is_known_shortcut_binding(app_settings, other_id) {
            continue;
        }
        if shortcuts_equivalent_for_implementation(
            binding,
            &other_binding.current_binding,
            app_settings.keyboard_implementation,
        ) {
            return Err(format!(
                "Shortcut '{}' is already assigned to '{}'",
                binding, other_binding.name
            ));
        }
    }
    Ok(())
}

fn ensure_preset_is_not_recording(
    app: &AppHandle,
    id: &str,
    operation: &str,
) -> Result<(), String> {
    if settings::active_transcription_preset_id(app).as_deref() == Some(id) {
        return Err(format!(
            "Cannot {} transcription preset '{}' while it is recording",
            operation, id
        ));
    }
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_binding(
    app: AppHandle,
    id: String,
    binding: String,
) -> Result<BindingResponse, String> {
    // Reject empty bindings — every shortcut should have a value
    if binding.trim().is_empty() {
        return Err("Binding cannot be empty".to_string());
    }

    let mut settings = settings::get_settings(&app);

    if settings::has_transcription_preset(&settings, &id) {
        ensure_preset_is_not_recording(&app, &id, "change the shortcut for")?;
    }

    // Get the binding to modify. A dynamic preset intentionally has no
    // binding until the user records one; that first chosen shortcut becomes
    // both its current binding and its reset/default value.
    let binding_to_modify = match settings.bindings.get(&id) {
        Some(binding) => binding.clone(),
        None => {
            if let Some(preset) = settings
                .transcription_presets
                .iter()
                .find(|preset| preset.id == id)
            {
                ShortcutBinding {
                    id: id.clone(),
                    name: format!("{} Shortcut", preset.name),
                    description: format!(
                        "Record using the '{}' transcription preset.",
                        preset.name
                    ),
                    default_binding: binding.clone(),
                    current_binding: binding.clone(),
                }
            } else {
                let default_settings = settings::get_default_settings();
                match default_settings.bindings.get(&id) {
                    Some(default_binding) => {
                        warn!(
                            "Binding '{}' not found in settings, creating from defaults",
                            id
                        );
                        default_binding.clone()
                    }
                    None => {
                        let error_msg = format!("Binding with id '{}' not found in defaults", id);
                        warn!("change_binding error: {}", error_msg);
                        return Ok(BindingResponse {
                            success: false,
                            binding: None,
                            error: Some(error_msg),
                        });
                    }
                }
            }
        }
    };

    // If this is the cancel binding, just update the settings and return
    // It's managed dynamically, so we don't register/unregister here
    if id == "cancel" {
        if let Some(mut b) = settings.bindings.get(&id).cloned() {
            b.current_binding = binding;
            settings.bindings.insert(id.clone(), b.clone());
            settings::write_settings(&app, settings);
            crate::secure_input::reconcile_fallback(&app);
            return Ok(BindingResponse {
                success: true,
                binding: Some(b.clone()),
                error: None,
            });
        }
    }

    // Validate before disturbing an existing native registration. Disabled
    // presets still receive full in-app conflict validation even though they
    // intentionally are not registered with the OS yet.
    if id != "cancel" {
        validate_shortcut_for_implementation(&binding, settings.keyboard_implementation)?;
        validate_binding_conflict(&settings, &id, &binding)?;
    }

    // Disabled preset shortcuts are persisted but intentionally not registered.
    // This lets users configure a preset completely before turning it on.
    if settings::has_transcription_preset(&settings, &id)
        && !settings::is_transcription_preset_enabled(&settings, &id)
    {
        let mut updated_binding = binding_to_modify;
        updated_binding.current_binding = binding;
        settings.bindings.insert(id, updated_binding.clone());
        settings::write_settings(&app, settings);
        return Ok(BindingResponse {
            success: true,
            binding: Some(updated_binding),
            error: None,
        });
    }

    // Unregister the existing binding. If teardown fails, keep settings and
    // runtime state unchanged instead of risking two live registrations.
    if let Err(e) = unregister_shortcut(&app, binding_to_modify.clone()) {
        let error_msg = format!("Failed to unregister shortcut: {}", e);
        error!("change_binding error: {}", error_msg);
        return Ok(BindingResponse {
            success: false,
            binding: None,
            error: Some(error_msg),
        });
    }

    // Create an updated binding
    let mut updated_binding = binding_to_modify.clone();
    updated_binding.current_binding = binding;

    // Register the new binding
    if let Err(e) = register_shortcut(&app, updated_binding.clone()) {
        let error_msg = format!("Failed to register shortcut: {}", e);
        error!("change_binding error: {}", error_msg);
        restore_registration(&app, &binding_to_modify);
        return Ok(BindingResponse {
            success: false,
            binding: None,
            error: Some(error_msg),
        });
    }

    // Update the binding in the settings
    settings.bindings.insert(id, updated_binding.clone());

    // Save the settings and synchronize any active Secure Input shadows.
    settings::write_settings(&app, settings);
    crate::secure_input::reconcile_fallback(&app);

    // Return the updated binding
    Ok(BindingResponse {
        success: true,
        binding: Some(updated_binding),
        error: None,
    })
}

/// Best-effort re-register of the previous binding after a failed change,
/// so a failure leaves the user's shortcut working exactly as before.
fn restore_registration(app: &AppHandle, binding: &ShortcutBinding) {
    if let Err(e) = register_shortcut(app, binding.clone()) {
        error!(
            "Failed to restore previous binding '{}' ({}): {}",
            binding.id, binding.current_binding, e
        );
    }
}

#[tauri::command]
#[specta::specta]
pub fn reset_binding(app: AppHandle, id: String) -> Result<BindingResponse, String> {
    let binding = settings::get_stored_binding(&settings::get_settings(&app), &id)?;
    change_binding(app, id, binding.default_binding)
}

/// Unregister every binding while the user is recording a new shortcut in
/// the UI, so no existing shortcut can fire — or swallow the keystrokes —
/// mid-capture. The "cancel" binding is untouched: it is managed dynamically
/// by the recording lifecycle.
pub fn suspend_all_shortcuts(app: &AppHandle) {
    for (id, binding) in settings::get_bindings(app) {
        if !should_unregister_during_bulk_cleanup(&id) {
            continue;
        }
        if let Err(e) = unregister_shortcut(app, binding) {
            debug!(
                "suspend_all_shortcuts: could not unregister '{}': {}",
                id, e
            );
        }
    }
}

/// Re-register every binding from settings after shortcut recording ends.
/// Registering an already-registered shortcut fails cleanly in both
/// implementations, so this is idempotent and safe on every exit path.
pub fn resume_all_shortcuts(app: &AppHandle) {
    let settings = get_settings(app);
    for (id, binding) in &settings.bindings {
        if id == "cancel" {
            continue;
        }
        if !settings::is_known_shortcut_binding(&settings, id) {
            continue;
        }
        if !settings::is_optional_shortcut_enabled(&settings, id) {
            continue;
        }
        if let Err(e) = register_shortcut(app, binding.clone()) {
            debug!("resume_all_shortcuts: could not register '{}': {}", id, e);
        }
    }
}

/// Temporarily unregister all bindings while the user is recording a
/// shortcut in the UI. This avoids firing actions while keys are recorded.
#[tauri::command]
#[specta::specta]
pub fn suspend_all_bindings(app: AppHandle) -> Result<(), String> {
    if settings::has_active_transcription_operation(&app) {
        return Err("Cannot record a new shortcut while transcription is recording".to_string());
    }
    suspend_all_shortcuts(&app);
    Ok(())
}

/// Re-register all bindings after the user has finished recording.
#[tauri::command]
#[specta::specta]
pub fn resume_all_bindings(app: AppHandle) -> Result<(), String> {
    resume_all_shortcuts(&app);
    Ok(())
}

fn should_unregister_during_bulk_cleanup(id: &str) -> bool {
    // `cancel` is dynamic. Every other known binding is safe to attempt to
    // unregister even when settings say it is disabled: settings can describe
    // desired state, not necessarily what the OS backend still has registered
    // after a partial failure.
    id != "cancel"
}

fn create_transcription_preset_in_settings(
    app_settings: &mut settings::AppSettings,
    id: String,
) -> Result<TranscriptionPreset, String> {
    if app_settings.transcription_presets.len() >= settings::MAX_TRANSCRIPTION_PRESETS {
        return Err(format!(
            "A maximum of {} transcription presets is supported",
            settings::MAX_TRANSCRIPTION_PRESETS
        ));
    }
    if settings::has_transcription_preset(app_settings, &id) {
        return Err(format!("Transcription preset '{}' already exists", id));
    }

    let preset = TranscriptionPreset {
        id,
        name: format!("Preset {}", app_settings.transcription_presets.len() + 1),
        enabled: false,
        model_id: String::new(),
        language: "auto".to_string(),
        translate_to_english: false,
        post_process: false,
        post_process_prompt_id: None,
    };
    app_settings.transcription_presets.push(preset.clone());
    Ok(preset)
}

#[tauri::command]
#[specta::specta]
pub fn create_transcription_preset(app: AppHandle) -> Result<TranscriptionPreset, String> {
    let mut app_settings = settings::get_settings(&app);
    let id = format!("preset_{}", Uuid::new_v4().simple());
    let mut preset = create_transcription_preset_in_settings(&mut app_settings, id)?;

    if !app_settings.selected_model.trim().is_empty() {
        let model_manager = app.state::<std::sync::Arc<crate::managers::model::ModelManager>>();
        if let Some(model) = model_manager.get_model_info(&app_settings.selected_model) {
            settings::reconcile_preset_model_capabilities(
                &mut preset,
                &model.supported_languages,
                model.supports_language_detection,
                model.supports_translation,
            );
            if let Some(stored) = app_settings
                .transcription_presets
                .iter_mut()
                .find(|stored| stored.id == preset.id)
            {
                *stored = preset.clone();
            }
        }
    }

    settings::write_settings(&app, app_settings);

    let _ = app.emit(
        "settings-changed",
        serde_json::json!({
            "setting": "transcription_presets",
            "value": preset
        }),
    );

    Ok(preset)
}

#[tauri::command]
#[specta::specta]
pub fn delete_transcription_preset(app: AppHandle, id: String) -> Result<(), String> {
    ensure_preset_is_not_recording(&app, &id, "delete")?;
    let mut app_settings = settings::get_settings(&app);
    let index = app_settings
        .transcription_presets
        .iter()
        .position(|preset| preset.id == id)
        .ok_or_else(|| format!("Transcription preset '{}' not found", id))?;
    let preset = app_settings.transcription_presets[index].clone();

    if let Some(binding) = app_settings.bindings.get(&id).cloned() {
        if preset.enabled {
            unregister_shortcut(&app, binding)?;
        } else if let Err(error) = unregister_shortcut(&app, binding) {
            debug!(
                "delete_transcription_preset: disabled shortcut '{}' was not registered: {}",
                id, error
            );
        }
    }

    app_settings.transcription_presets.remove(index);
    app_settings.bindings.remove(&id);
    settings::write_settings(&app, app_settings);
    crate::secure_input::reconcile_fallback(&app);

    let _ = app.emit(
        "settings-changed",
        serde_json::json!({
            "setting": "transcription_presets",
            "deleted_id": id
        }),
    );

    Ok(())
}

/// Update one dynamic transcription preset. Shortcut editing stays in the
/// existing `change_binding` command; this command owns the preset's
/// model/language/translation/post-processing metadata and enable state.
#[tauri::command]
#[specta::specta]
pub fn update_transcription_preset(
    app: AppHandle,
    mut preset: TranscriptionPreset,
) -> Result<(), String> {
    preset.name = preset.name.trim().to_string();
    if preset.name.is_empty() {
        return Err("Preset name cannot be empty".to_string());
    }
    if preset.language.trim().is_empty() {
        preset.language = "auto".to_string();
    }

    let mut app_settings = settings::get_settings(&app);
    if !settings::has_transcription_preset(&app_settings, &preset.id) {
        return Err(format!("Unknown transcription preset id: {}", preset.id));
    }

    // A preset must never persist a model that cannot actually be used. For
    // "Use current model", validate the effective model when the preset is
    // enabled; explicit model selections are validated on every save.
    let effective_model_id = if preset.model_id.trim().is_empty() {
        app_settings.selected_model.clone()
    } else {
        preset.model_id.clone()
    };
    let must_validate_model = !preset.model_id.trim().is_empty() || preset.enabled;
    if must_validate_model && effective_model_id.trim().is_empty() {
        return Err("Preset requires a downloaded transcription model".to_string());
    }

    if !effective_model_id.trim().is_empty() {
        let model_manager = app.state::<std::sync::Arc<crate::managers::model::ModelManager>>();
        match model_manager.get_model_info(&effective_model_id) {
            Some(model) => {
                if must_validate_model && !model.is_downloaded {
                    return Err(format!("Model not downloaded: {}", effective_model_id));
                }
                settings::reconcile_preset_model_capabilities(
                    &mut preset,
                    &model.supported_languages,
                    model.supports_language_detection,
                    model.supports_translation,
                );
            }
            None if must_validate_model => {
                return Err(format!("Model not found: {}", effective_model_id));
            }
            None => {}
        }
    }

    let index = app_settings
        .transcription_presets
        .iter()
        .position(|existing| existing.id == preset.id)
        .ok_or_else(|| format!("Preset slot '{}' is missing from settings", preset.id))?;
    let was_enabled = app_settings.transcription_presets[index].enabled;
    let was_post_process = app_settings.transcription_presets[index].post_process;

    if was_enabled && !preset.enabled {
        ensure_preset_is_not_recording(&app, &preset.id, "disable")?;
    }

    if preset.post_process {
        if !app_settings.post_process_enabled && !was_post_process {
            return Err(
                "Global AI post-processing is disabled; enable it before enabling preset post-processing"
                    .to_string(),
            );
        }
        settings::validate_preset_post_process_configuration(
            &app_settings,
            preset.post_process_prompt_id.as_deref(),
        )?;
    } else if preset
        .post_process_prompt_id
        .as_deref()
        .is_some_and(|prompt_id| {
            !app_settings
                .post_process_prompts
                .iter()
                .any(|prompt| prompt.id == prompt_id)
        })
    {
        preset.post_process_prompt_id = None;
    }
    let binding = app_settings.bindings.get(&preset.id).cloned();
    if preset.enabled && binding.is_none() {
        return Err("Add a shortcut before enabling this preset".to_string());
    }

    if preset.enabled && !was_enabled {
        register_shortcut(&app, binding.expect("enabled preset binding checked above"))?;
    } else if !preset.enabled && was_enabled {
        if let Some(binding) = binding {
            unregister_shortcut(&app, binding)?;
        } else {
            warn!(
                "Enabled transcription preset '{}' had no shortcut binding while disabling",
                preset.id
            );
        }
    }

    app_settings.transcription_presets[index] = preset.clone();
    if let Some(binding) = app_settings.bindings.get_mut(&preset.id) {
        binding.name = format!("{} Shortcut", preset.name);
        binding.description = format!("Record using the '{}' transcription preset.", preset.name);
    }
    settings::write_settings(&app, app_settings);
    crate::secure_input::reconcile_fallback(&app);

    let _ = app.emit(
        "settings-changed",
        serde_json::json!({
            "setting": "transcription_presets",
            "value": preset
        }),
    );

    Ok(())
}

// ============================================================================
// Keyboard Implementation Switching
// ============================================================================

/// Result of changing keyboard implementation
#[derive(Serialize, Type)]
pub struct ImplementationChangeResult {
    pub success: bool,
    /// List of binding IDs that were reset to defaults due to incompatibility
    pub reset_bindings: Vec<String>,
}

/// Change the keyboard implementation with runtime switching.
/// This will unregister all shortcuts from the old implementation,
/// validate shortcuts for the new implementation (resetting invalid ones to defaults),
/// and register them with the new implementation.
#[tauri::command]
#[specta::specta]
pub fn change_keyboard_implementation_setting(
    app: AppHandle,
    implementation: String,
) -> Result<ImplementationChangeResult, String> {
    let current_settings = settings::get_settings(&app);
    let current_impl = current_settings.keyboard_implementation;
    let new_impl = parse_keyboard_implementation(&implementation);

    if current_impl == new_impl {
        return Ok(ImplementationChangeResult {
            success: true,
            reset_bindings: vec![],
        });
    }

    if settings::has_active_transcription_operation(&app) {
        return Err("Cannot switch keyboard implementation while recording".to_string());
    }

    validate_enabled_preset_shortcuts_for_implementation(&current_settings, new_impl)?;
    let (mut target_settings, reset_bindings) =
        prepare_settings_for_implementation(&current_settings, new_impl)?;
    target_settings.keyboard_implementation = new_impl;

    info!(
        "Switching keyboard implementation from {:?} to {:?}",
        current_impl, new_impl
    );

    let old_bindings = unregister_bindings_for_implementation(
        &app,
        current_impl,
        &eligible_shortcut_bindings(&current_settings),
    )?;

    // Carbon Secure Input shadows use the Tauri backend while HandyKeys is
    // active. Remove them explicitly before attempting a Tauri switch without
    // changing persisted settings. If this fails, restore the old backend and
    // leave the user's selected implementation untouched.
    if new_impl == KeyboardImplementation::Tauri {
        if let Err(error) = crate::secure_input::suspend_fallback_for_backend_switch(&app) {
            if let Err(restore_error) =
                restore_bindings_for_implementation(&app, current_impl, &old_bindings)
            {
                error!(
                    "Keyboard implementation rollback could not fully restore {:?}: {}",
                    current_impl, restore_error
                );
            }
            crate::secure_input::reconcile_fallback(&app);
            return Err(format!(
                "Failed to prepare Secure Input fallback for {:?}: {}",
                new_impl, error
            ));
        }
    }

    let target_result = if new_impl == KeyboardImplementation::HandyKeys
        && app.try_state::<handy_keys::HandyKeysState>().is_none()
    {
        handy_keys::init_shortcuts_with_settings(&app, &target_settings).map(|_| Vec::new())
    } else {
        register_bindings_for_implementation(
            &app,
            new_impl,
            &eligible_shortcut_bindings(&target_settings),
        )
    };

    if let Err(error) = target_result {
        if let Err(restore_error) =
            restore_bindings_for_implementation(&app, current_impl, &old_bindings)
        {
            error!(
                "Keyboard implementation rollback could not fully restore {:?}: {}",
                current_impl, restore_error
            );
        }
        crate::secure_input::reconcile_fallback(&app);
        return Err(format!(
            "Failed to switch keyboard implementation to {:?}: {}. Restored {:?}.",
            new_impl, error, current_impl
        ));
    }

    // Native target registration has succeeded. Persist the new implementation
    // and any compatibility resets only now, so handled registration failures
    // never leave target settings on disk.
    settings::write_settings(&app, target_settings);
    crate::secure_input::reconcile_fallback(&app);

    let _ = app.emit(
        "settings-changed",
        serde_json::json!({
            "setting": "keyboard_implementation",
            "value": implementation,
            "reset_bindings": reset_bindings
        }),
    );

    info!("Keyboard implementation switched to {:?}", new_impl);

    Ok(ImplementationChangeResult {
        success: true,
        reset_bindings,
    })
}

/// Get the current keyboard implementation
#[tauri::command]
#[specta::specta]
pub fn get_keyboard_implementation(app: AppHandle) -> String {
    let settings = settings::get_settings(&app);
    match settings.keyboard_implementation {
        KeyboardImplementation::Tauri => "tauri".to_string(),
        KeyboardImplementation::HandyKeys => "handy_keys".to_string(),
    }
}

// ============================================================================
// Validation Helpers
// ============================================================================

/// Validate a shortcut for a specific implementation
fn validate_shortcut_for_implementation(
    raw: &str,
    implementation: KeyboardImplementation,
) -> Result<(), String> {
    match implementation {
        KeyboardImplementation::Tauri => tauri_impl::validate_shortcut(raw),
        KeyboardImplementation::HandyKeys => handy_keys::validate_shortcut(raw),
    }
}

fn validate_enabled_preset_shortcuts_for_implementation(
    app_settings: &settings::AppSettings,
    implementation: KeyboardImplementation,
) -> Result<(), String> {
    for preset in app_settings
        .transcription_presets
        .iter()
        .filter(|preset| preset.enabled)
    {
        let binding = app_settings.bindings.get(&preset.id).ok_or_else(|| {
            format!(
                "Enabled transcription preset '{}' has no shortcut",
                preset.name
            )
        })?;

        if validate_shortcut_for_implementation(&binding.current_binding, implementation).is_ok() {
            continue;
        }

        if validate_shortcut_for_implementation(&binding.default_binding, implementation).is_ok() {
            continue;
        }

        return Err(format!(
            "Preset '{}' uses shortcut '{}' which is not supported by {:?}. Change the preset shortcut before switching keyboard implementation.",
            preset.name, binding.current_binding, implementation
        ));
    }

    Ok(())
}

/// Parse a keyboard implementation string into the enum
fn parse_keyboard_implementation(s: &str) -> KeyboardImplementation {
    match s {
        "tauri" => KeyboardImplementation::Tauri,
        "handy_keys" => KeyboardImplementation::HandyKeys,
        other => {
            warn!(
                "Invalid keyboard implementation '{}', defaulting to tauri",
                other
            );
            KeyboardImplementation::Tauri
        }
    }
}

fn eligible_shortcut_bindings(app_settings: &settings::AppSettings) -> Vec<ShortcutBinding> {
    let mut bindings = app_settings
        .bindings
        .iter()
        .filter(|(id, _)| id.as_str() != "cancel")
        .filter(|(id, _)| settings::is_known_shortcut_binding(app_settings, id.as_str()))
        .filter(|(id, _)| settings::is_optional_shortcut_enabled(app_settings, id.as_str()))
        .map(|(_, binding)| binding.clone())
        .collect::<Vec<_>>();
    bindings.sort_by(|left, right| left.id.cmp(&right.id));
    bindings
}

fn prepare_settings_for_implementation(
    current_settings: &settings::AppSettings,
    implementation: KeyboardImplementation,
) -> Result<(settings::AppSettings, Vec<String>), String> {
    let default_bindings = settings::get_default_settings().bindings;
    let mut target_settings = current_settings.clone();
    let mut reset_bindings = Vec::new();

    for binding in eligible_shortcut_bindings(current_settings) {
        if validate_shortcut_for_implementation(&binding.current_binding, implementation).is_ok() {
            continue;
        }

        let fallback = default_bindings
            .get(&binding.id)
            .map(|default| default.current_binding.clone())
            .unwrap_or_else(|| binding.default_binding.clone());
        validate_shortcut_for_implementation(&fallback, implementation).map_err(|error| {
            format!(
                "Shortcut '{}' is incompatible with {:?} and has no compatible default: {}",
                binding.id, implementation, error
            )
        })?;

        if let Some(target_binding) = target_settings.bindings.get_mut(&binding.id) {
            target_binding.current_binding = fallback;
            reset_bindings.push(binding.id);
        }
    }

    Ok((target_settings, reset_bindings))
}

fn register_binding_for_implementation(
    app: &AppHandle,
    implementation: KeyboardImplementation,
    binding: ShortcutBinding,
) -> Result<(), String> {
    match implementation {
        KeyboardImplementation::Tauri => tauri_impl::register_shortcut(app, binding),
        KeyboardImplementation::HandyKeys => handy_keys::register_shortcut(app, binding),
    }
}

fn unregister_binding_for_implementation(
    app: &AppHandle,
    implementation: KeyboardImplementation,
    binding: ShortcutBinding,
) -> Result<(), String> {
    match implementation {
        KeyboardImplementation::Tauri => tauri_impl::unregister_shortcut(app, binding),
        KeyboardImplementation::HandyKeys => handy_keys::unregister_shortcut(app, binding),
    }
}

fn restore_bindings_for_implementation(
    app: &AppHandle,
    implementation: KeyboardImplementation,
    bindings: &[ShortcutBinding],
) -> Result<(), String> {
    let mut failures = Vec::new();
    for binding in bindings {
        if let Err(error) =
            register_binding_for_implementation(app, implementation, binding.clone())
        {
            failures.push(format!("{}: {}", binding.id, error));
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

fn unregister_bindings_for_implementation(
    app: &AppHandle,
    implementation: KeyboardImplementation,
    bindings: &[ShortcutBinding],
) -> Result<Vec<ShortcutBinding>, String> {
    let mut unregistered = Vec::new();
    for binding in bindings {
        if let Err(error) =
            unregister_binding_for_implementation(app, implementation, binding.clone())
        {
            let _ = restore_bindings_for_implementation(app, implementation, &unregistered);
            return Err(format!(
                "Failed to unregister '{}' while switching keyboard implementation: {}",
                binding.id, error
            ));
        }
        unregistered.push(binding.clone());
    }
    Ok(unregistered)
}

fn register_bindings_for_implementation(
    app: &AppHandle,
    implementation: KeyboardImplementation,
    bindings: &[ShortcutBinding],
) -> Result<Vec<ShortcutBinding>, String> {
    let mut registered: Vec<ShortcutBinding> = Vec::new();
    for binding in bindings {
        if let Err(error) =
            register_binding_for_implementation(app, implementation, binding.clone())
        {
            for registered_binding in registered.iter().rev() {
                let _ = unregister_binding_for_implementation(
                    app,
                    implementation,
                    registered_binding.clone(),
                );
            }
            return Err(format!(
                "Failed to register '{}' while switching keyboard implementation: {}",
                binding.id, error
            ));
        }
        registered.push(binding.clone());
    }
    Ok(registered)
}

// ============================================================================
// General Settings Commands
// ============================================================================

#[tauri::command]
#[specta::specta]
pub fn change_shortcut_activation_setting(
    app: AppHandle,
    activation: ShortcutActivation,
) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.shortcut_activation = activation;
    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_hold_threshold_ms_setting(app: AppHandle, ms: u64) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.hold_threshold_ms = ms;
    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_audio_feedback_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.audio_feedback = enabled;
    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_audio_feedback_volume_setting(app: AppHandle, volume: f32) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.audio_feedback_volume = volume;
    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_sound_theme_setting(app: AppHandle, theme: String) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    let parsed = match theme.as_str() {
        "marimba" => SoundTheme::Marimba,
        "pop" => SoundTheme::Pop,
        "custom" => SoundTheme::Custom,
        other => {
            warn!("Invalid sound theme '{}', defaulting to marimba", other);
            SoundTheme::Marimba
        }
    };
    settings.sound_theme = parsed;
    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_theme_setting(app: AppHandle, theme: String) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    let parsed = match theme.as_str() {
        "system" => Theme::System,
        "light" => Theme::Light,
        "dark" => Theme::Dark,
        other => {
            warn!("Invalid theme '{}', defaulting to system", other);
            Theme::System
        }
    };
    settings.theme = parsed;
    settings::write_settings(&app, settings);
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    apply_window_theme(&app, parsed);
    // Notify other webviews (the recording overlay) so they re-apply the palette
    // live — they set `data-theme` on their own document and can't see this one.
    let _ = app.emit("theme-changed", parsed);
    Ok(())
}

/// Applies the appearance setting to the native window chrome (title bar), which
/// CSS `data-theme` cannot reach. `System` clears the override so the window
/// follows the OS. Call this on startup and whenever the setting changes to keep
/// the title bar in sync with the in-app palette.
///
/// On Windows this themes the title bar only. On macOS `set_theme` sets
/// `NSApp.appearance` app-wide, which is what we want here: it darkens the title
/// bar and keeps the overlay in step. Linux is left to `data-theme` alone, since
/// its window theming is backend-dependent and unreliable.
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub fn apply_window_theme(app: &AppHandle, theme: Theme) {
    let window_theme = match theme {
        Theme::System => None,
        Theme::Light => Some(tauri::Theme::Light),
        Theme::Dark => Some(tauri::Theme::Dark),
    };
    if let Some(window) = app.get_webview_window("main") {
        if let Err(e) = window.set_theme(window_theme) {
            warn!("Failed to apply window theme: {}", e);
        }
    }
}

#[tauri::command]
#[specta::specta]
pub fn change_translate_to_english_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.translate_to_english = enabled;
    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_selected_language_setting(app: AppHandle, language: String) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.selected_language = language;
    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_overlay_position_setting(app: AppHandle, position: String) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    let parsed = match position.as_str() {
        // "none" is retired (visibility is overlay_style now); fold legacy callers
        // onto Bottom rather than warn.
        "none" | "bottom" => OverlayPosition::Bottom,
        "top" => OverlayPosition::Top,
        other => {
            warn!("Invalid overlay position '{}', defaulting to bottom", other);
            OverlayPosition::Bottom
        }
    };
    settings.overlay_position = parsed;
    settings::write_settings(&app, settings);

    // Whether the overlay shows at all is owned by overlay_style now; position
    // only ever toggles Top/Bottom, so the enabled cache is untouched here.
    // Update overlay position without recreating window
    crate::utils::update_overlay_position(&app);

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_overlay_style_setting(app: AppHandle, style: String) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    let parsed = match style.as_str() {
        "none" => OverlayStyle::None,
        "minimal" => OverlayStyle::Minimal,
        "live" => OverlayStyle::Live,
        other => {
            warn!("Invalid overlay style '{}', defaulting to minimal", other);
            OverlayStyle::Minimal
        }
    };
    settings.overlay_style = parsed;
    settings::write_settings(&app, settings);

    // Keep the cached overlay-enabled flag in sync so emit_levels stops (or
    // resumes) emitting on the next audio callback.
    crate::overlay::update_overlay_enabled_cache(parsed != OverlayStyle::None);

    // Reposition in case the window needs to re-center for the new style.
    crate::utils::update_overlay_position(&app);

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_debug_mode_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.debug_mode = enabled;
    settings::write_settings(&app, settings);

    // Keep webview log streaming in sync: the live log viewer only exists in
    // debug mode, so logs are forwarded to the frontend only while it is on.
    crate::WEBVIEW_LOG_STREAMING.store(enabled, std::sync::atomic::Ordering::Relaxed);

    // Emit event to notify frontend of debug mode change
    let _ = app.emit(
        "settings-changed",
        serde_json::json!({
            "setting": "debug_mode",
            "value": enabled
        }),
    );

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_start_hidden_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.start_hidden = enabled;
    settings::write_settings(&app, settings);

    // Notify frontend
    let _ = app.emit(
        "settings-changed",
        serde_json::json!({
            "setting": "start_hidden",
            "value": enabled
        }),
    );

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_autostart_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.autostart_enabled = enabled;
    settings::write_settings(&app, settings);

    // Apply the autostart setting immediately
    crate::autostart::apply_autostart(&app, enabled);

    // Notify frontend
    let _ = app.emit(
        "settings-changed",
        serde_json::json!({
            "setting": "autostart_enabled",
            "value": enabled
        }),
    );

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_update_checks_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    if settings::update_checks_forced_disabled() {
        return Err(
            "Update checks are disabled by system configuration (HANDY_DISABLE_UPDATER)".into(),
        );
    }

    let mut settings = settings::get_settings(&app);
    settings.update_checks_enabled = enabled;
    settings::write_settings(&app, settings);

    let _ = app.emit(
        "settings-changed",
        serde_json::json!({
            "setting": "update_checks_enabled",
            "value": enabled
        }),
    );

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_show_whats_new_on_update_setting(
    app: AppHandle,
    enabled: bool,
) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.show_whats_new_on_update = enabled;
    settings::write_settings(&app, settings);

    let _ = app.emit(
        "settings-changed",
        serde_json::json!({
            "setting": "show_whats_new_on_update",
            "value": enabled
        }),
    );

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_whats_new_last_seen_version_setting(
    app: AppHandle,
    version: String,
) -> Result<(), String> {
    let version = version.trim().to_string();
    let mut settings = settings::get_settings(&app);
    settings.whats_new_last_seen_version = version.clone();
    settings::write_settings(&app, settings);

    let _ = app.emit(
        "settings-changed",
        serde_json::json!({
            "setting": "whats_new_last_seen_version",
            "value": version
        }),
    );

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn update_custom_words(app: AppHandle, words: Vec<String>) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.custom_words = words;
    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_word_correction_threshold_setting(
    app: AppHandle,
    threshold: f64,
) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.word_correction_threshold = threshold;
    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_extra_recording_buffer_setting(app: AppHandle, ms: u64) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.extra_recording_buffer_ms = ms;
    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_paste_delay_ms_setting(app: AppHandle, ms: u64) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.paste_delay_ms = ms;
    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_paste_delay_after_ms_setting(app: AppHandle, ms: u64) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.paste_delay_after_ms = ms;
    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_reliable_paste_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.reliable_paste = enabled;
    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_paste_method_setting(app: AppHandle, method: String) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    let parsed = match method.as_str() {
        "ctrl_v" => PasteMethod::CtrlV,
        "direct" => PasteMethod::Direct,
        "none" => PasteMethod::None,
        "shift_insert" => PasteMethod::ShiftInsert,
        "ctrl_shift_v" => PasteMethod::CtrlShiftV,
        "external_script" => PasteMethod::ExternalScript,
        other => {
            warn!("Invalid paste method '{}', defaulting to ctrl_v", other);
            PasteMethod::CtrlV
        }
    };
    settings.paste_method = parsed;
    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn get_available_typing_tools() -> Vec<String> {
    #[cfg(target_os = "linux")]
    {
        crate::clipboard::get_available_typing_tools()
    }
    #[cfg(not(target_os = "linux"))]
    {
        vec!["auto".to_string()]
    }
}

#[tauri::command]
#[specta::specta]
pub fn change_typing_tool_setting(app: AppHandle, tool: String) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    let parsed = match tool.as_str() {
        "auto" => TypingTool::Auto,
        "wtype" => TypingTool::Wtype,
        "kwtype" => TypingTool::Kwtype,
        "dotool" => TypingTool::Dotool,
        "ydotool" => TypingTool::Ydotool,
        "xdotool" => TypingTool::Xdotool,
        other => {
            warn!("Invalid typing tool '{}', defaulting to auto", other);
            TypingTool::Auto
        }
    };
    settings.typing_tool = parsed;
    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_external_script_path_setting(
    app: AppHandle,
    path: Option<String>,
) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.external_script_path = path;
    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_clipboard_handling_setting(app: AppHandle, handling: String) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    let parsed = match handling.as_str() {
        "dont_modify" => ClipboardHandling::DontModify,
        "copy_to_clipboard" => ClipboardHandling::CopyToClipboard,
        other => {
            warn!(
                "Invalid clipboard handling '{}', defaulting to dont_modify",
                other
            );
            ClipboardHandling::DontModify
        }
    };
    settings.clipboard_handling = parsed;
    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_auto_submit_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.auto_submit = enabled;
    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_auto_submit_key_setting(app: AppHandle, key: String) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    let parsed = match key.as_str() {
        "enter" => AutoSubmitKey::Enter,
        "ctrl_enter" => AutoSubmitKey::CtrlEnter,
        "cmd_enter" => AutoSubmitKey::CmdEnter,
        other => {
            warn!("Invalid auto submit key '{}', defaulting to enter", other);
            AutoSubmitKey::Enter
        }
    };
    settings.auto_submit_key = parsed;
    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_post_process_enabled_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.post_process_enabled = enabled;
    settings::write_settings(&app, settings.clone());

    // Register or unregister the post-processing shortcut
    if let Some(binding) = settings
        .bindings
        .get("transcribe_with_post_process")
        .cloned()
    {
        if enabled {
            let _ = register_shortcut(&app, binding);
        } else {
            let _ = unregister_shortcut(&app, binding);
        }
    }

    crate::secure_input::reconcile_fallback(&app);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_experimental_enabled_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.experimental_enabled = enabled;
    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_post_process_base_url_setting(
    app: AppHandle,
    provider_id: String,
    base_url: String,
) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    let label = settings
        .post_process_provider(&provider_id)
        .map(|provider| provider.label.clone())
        .ok_or_else(|| format!("Provider '{}' not found", provider_id))?;

    let provider = settings
        .post_process_provider_mut(&provider_id)
        .expect("Provider looked up above must exist");

    if provider.id != "custom" {
        return Err(format!(
            "Provider '{}' does not allow editing the base URL",
            label
        ));
    }

    provider.base_url = base_url;
    settings::write_settings(&app, settings);
    Ok(())
}

/// Generic helper to validate provider exists
fn validate_provider_exists(
    settings: &settings::AppSettings,
    provider_id: &str,
) -> Result<(), String> {
    if !settings
        .post_process_providers
        .iter()
        .any(|provider| provider.id == provider_id)
    {
        return Err(format!("Provider '{}' not found", provider_id));
    }
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_post_process_api_key_setting(
    app: AppHandle,
    provider_id: String,
    api_key: String,
) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    validate_provider_exists(&settings, &provider_id)?;
    settings.post_process_api_keys.insert(provider_id, api_key);
    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_post_process_model_setting(
    app: AppHandle,
    provider_id: String,
    model: String,
) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    validate_provider_exists(&settings, &provider_id)?;
    settings.post_process_models.insert(provider_id, model);
    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn set_post_process_provider(app: AppHandle, provider_id: String) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    validate_provider_exists(&settings, &provider_id)?;
    settings.post_process_provider_id = provider_id;
    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn add_post_process_prompt(
    app: AppHandle,
    name: String,
    prompt: String,
) -> Result<LLMPrompt, String> {
    let mut settings = settings::get_settings(&app);

    // Generate unique ID using timestamp and random component
    let id = format!("prompt_{}", chrono::Utc::now().timestamp_millis());

    let new_prompt = LLMPrompt {
        id: id.clone(),
        name,
        prompt,
    };

    settings.post_process_prompts.push(new_prompt.clone());
    settings::write_settings(&app, settings);

    Ok(new_prompt)
}

#[tauri::command]
#[specta::specta]
pub fn update_post_process_prompt(
    app: AppHandle,
    id: String,
    name: String,
    prompt: String,
) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);

    if let Some(existing_prompt) = settings
        .post_process_prompts
        .iter_mut()
        .find(|p| p.id == id)
    {
        existing_prompt.name = name;
        existing_prompt.prompt = prompt;
        settings::write_settings(&app, settings);
        Ok(())
    } else {
        Err(format!("Prompt with id '{}' not found", id))
    }
}

fn reconcile_presets_for_deleted_prompt(settings: &mut settings::AppSettings, id: &str) {
    for preset in &mut settings.transcription_presets {
        if preset.post_process_prompt_id.as_deref() == Some(id) {
            preset.post_process = false;
            preset.post_process_prompt_id = None;
        }
    }
}

#[tauri::command]
#[specta::specta]
pub fn delete_post_process_prompt(app: AppHandle, id: String) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);

    // Don't allow deleting the last prompt
    if settings.post_process_prompts.len() <= 1 {
        return Err("Cannot delete the last prompt".to_string());
    }

    // Find and remove the prompt
    let original_len = settings.post_process_prompts.len();
    settings.post_process_prompts.retain(|p| p.id != id);

    if settings.post_process_prompts.len() == original_len {
        return Err(format!("Prompt with id '{}' not found", id));
    }

    // If the deleted prompt was selected, select the first one or None. Presets
    // referencing it are explicitly turned off for post-processing so they can
    // never claim to polish text while silently doing nothing.
    if settings.post_process_selected_prompt_id.as_ref() == Some(&id) {
        settings.post_process_selected_prompt_id =
            settings.post_process_prompts.first().map(|p| p.id.clone());
    }
    reconcile_presets_for_deleted_prompt(&mut settings, &id);

    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn fetch_post_process_models(
    app: AppHandle,
    provider_id: String,
) -> Result<Vec<String>, String> {
    let settings = settings::get_settings(&app);

    // Find the provider
    let provider = settings
        .post_process_providers
        .iter()
        .find(|p| p.id == provider_id)
        .ok_or_else(|| format!("Provider '{}' not found", provider_id))?;

    if provider.id == APPLE_INTELLIGENCE_PROVIDER_ID {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            return Ok(vec![APPLE_INTELLIGENCE_DEFAULT_MODEL_ID.to_string()]);
        }

        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        {
            return Err("Apple Intelligence is only available on Apple silicon Macs running macOS 15 or later.".to_string());
        }
    }

    // Get API key
    let api_key = settings
        .post_process_api_keys
        .get(&provider_id)
        .cloned()
        .unwrap_or_default();

    // Skip fetching if no API key for providers that typically need one
    if api_key.trim().is_empty() && provider.id != "custom" {
        return Err(format!(
            "API key is required for {}. Please add an API key to list available models.",
            provider.label
        ));
    }

    crate::llm_client::fetch_models(provider, api_key).await
}

#[tauri::command]
#[specta::specta]
pub fn set_post_process_selected_prompt(app: AppHandle, id: String) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);

    // Verify the prompt exists
    if !settings.post_process_prompts.iter().any(|p| p.id == id) {
        return Err(format!("Prompt with id '{}' not found", id));
    }

    settings.post_process_selected_prompt_id = Some(id);
    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_mute_while_recording_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.mute_while_recording = enabled;
    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_append_trailing_space_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.append_trailing_space = enabled;
    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_lazy_stream_close_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.lazy_stream_close = enabled;
    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_vad_enabled_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.vad_enabled = enabled;
    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn change_vad_backend_setting(app: AppHandle, backend: VadBackend) -> Result<(), String> {
    if settings::get_settings(&app).vad_backend == backend {
        return Ok(());
    }

    // Construct/swap the detector and, when necessary, reopen cpal away from
    // the webview thread. Persist only after the runtime change succeeds so a
    // rejected in-progress switch or failed microphone reopen rolls back cleanly.
    let manager = app
        .state::<std::sync::Arc<crate::managers::audio::AudioRecordingManager>>()
        .inner()
        .clone();
    tokio::task::spawn_blocking(move || manager.update_vad_backend(backend))
        .await
        .map_err(|e| format!("audio task join failed: {e}"))?
        .map_err(|e| format!("Failed to update VAD backend: {e}"))?;

    let mut current_settings = settings::get_settings(&app);
    current_settings.vad_backend = backend;
    settings::write_settings(&app, current_settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_filler_word_removal_enabled_setting(
    app: AppHandle,
    enabled: bool,
) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.filler_word_removal_enabled = enabled;
    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_app_language_setting(app: AppHandle, language: String) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.app_language = language.clone();
    settings::write_settings(&app, settings);

    // Refresh the tray menu with the new language
    tray::update_tray_menu(&app);

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_show_tray_icon_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.show_tray_icon = enabled;
    settings::write_settings(&app, settings);

    // Apply change immediately
    tray::set_tray_visibility(&app, enabled);

    Ok(())
}

/// Save accelerator settings and make the next model use reload with them.
/// The currently running transcription, if any, keeps its existing engine.
fn save_accelerator_and_reload_next_use(app: &AppHandle, s: settings::AppSettings) {
    settings::write_settings(app, s);

    let tm = app.state::<std::sync::Arc<crate::managers::transcription::TranscriptionManager>>();
    tm.reload_model_on_next_use();
}

#[tauri::command]
#[specta::specta]
pub fn change_transcribe_accelerator_setting(
    app: AppHandle,
    accelerator: settings::TranscribeAcceleratorSetting,
) -> Result<(), String> {
    let mut s = settings::get_settings(&app);
    s.transcribe_accelerator = accelerator;
    save_accelerator_and_reload_next_use(&app, s);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_ort_accelerator_setting(
    app: AppHandle,
    accelerator: settings::OrtAcceleratorSetting,
) -> Result<(), String> {
    let mut s = settings::get_settings(&app);
    s.ort_accelerator = accelerator;
    save_accelerator_and_reload_next_use(&app, s);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_transcribe_gpu_device(app: AppHandle, device: Option<String>) -> Result<(), String> {
    let mut s = settings::get_settings(&app);
    s.transcribe_gpu_device = device;
    save_accelerator_and_reload_next_use(&app, s);
    Ok(())
}

/// Return which accelerators and GPU devices are available for this build.
///
/// First-call cost is dominated by enumerating GPU devices through the
/// transcribe.cpp Metal/Vulkan backend, which loads dynamic libraries and
/// probes hardware. Run it on the blocking pool so the webview thread
/// stays responsive — see also the startup pre-warm in `lib.rs`.
#[tauri::command]
#[specta::specta]
pub async fn get_available_accelerators() -> crate::managers::transcription::AvailableAccelerators {
    tauri::async_runtime::spawn_blocking(crate::managers::transcription::get_available_accelerators)
        .await
        .expect("get_available_accelerators panicked")
}

#[cfg(test)]
mod tests {
    use handy_keys::Hotkey;
    use tauri_plugin_global_shortcut::Shortcut;

    #[test]
    fn compound_shortcut_keys_parse_on_both_backends() {
        for key in [
            "scrolllock",
            "capslock",
            "numlock",
            "pageup",
            "pagedown",
            "printscreen",
        ] {
            assert!(key.parse::<Shortcut>().is_ok(), "Tauri rejected {key}");
            assert!(key.parse::<Hotkey>().is_ok(), "HandyKeys rejected {key}");
        }
    }
}

#[cfg(test)]
mod preset_tests {
    use super::{
        create_transcription_preset_in_settings, prepare_settings_for_implementation,
        reconcile_presets_for_deleted_prompt, should_unregister_during_bulk_cleanup,
        validate_binding_conflict, validate_enabled_preset_shortcuts_for_implementation,
    };
    use crate::settings::{
        self, get_default_settings, normalize_preset_language_for_model, KeyboardImplementation,
        ShortcutBinding, TranscriptionPreset,
    };

    fn test_preset(id: &str) -> TranscriptionPreset {
        TranscriptionPreset {
            id: id.to_string(),
            name: id.to_string(),
            enabled: false,
            model_id: String::new(),
            language: "auto".to_string(),
            translate_to_english: false,
            post_process: false,
            post_process_prompt_id: None,
        }
    }

    #[test]
    fn preset_language_normalization_never_persists_unsupported_auto() {
        let languages = vec!["en-US".to_string(), "nl-NL".to_string()];

        assert_eq!(
            normalize_preset_language_for_model("auto", &languages, false),
            "en"
        );
        assert_eq!(
            normalize_preset_language_for_model("nl", &languages, false),
            "nl"
        );
        assert_eq!(
            normalize_preset_language_for_model("de", &languages, true),
            "auto"
        );
        assert_eq!(
            normalize_preset_language_for_model("auto", &["zh".to_string()], false),
            "zh-Hans"
        );
    }

    #[test]
    fn bulk_cleanup_includes_disabled_preset_bindings() {
        assert!(should_unregister_during_bulk_cleanup("preset_1"));
        assert!(should_unregister_during_bulk_cleanup("transcribe"));
        assert!(!should_unregister_during_bulk_cleanup("cancel"));
    }

    #[test]
    fn backend_switch_rejects_enabled_preset_without_compatible_shortcut() {
        let mut app_settings = get_default_settings();
        let mut preset = test_preset("preset_modifier_only");
        preset.enabled = true;
        app_settings.transcription_presets.push(preset);
        app_settings.bindings.insert(
            "preset_modifier_only".to_string(),
            ShortcutBinding {
                id: "preset_modifier_only".to_string(),
                name: "Modifier only".to_string(),
                description: "Test preset shortcut".to_string(),
                default_binding: "shift".to_string(),
                current_binding: "shift".to_string(),
            },
        );

        assert!(validate_enabled_preset_shortcuts_for_implementation(
            &app_settings,
            KeyboardImplementation::Tauri
        )
        .is_err());
        assert!(validate_enabled_preset_shortcuts_for_implementation(
            &app_settings,
            KeyboardImplementation::HandyKeys
        )
        .is_ok());

        let binding = app_settings
            .bindings
            .get_mut("preset_modifier_only")
            .unwrap();
        binding.default_binding = "ctrl+space".to_string();

        assert!(validate_enabled_preset_shortcuts_for_implementation(
            &app_settings,
            KeyboardImplementation::Tauri
        )
        .is_ok());
    }

    #[test]
    fn disabled_preset_binding_rejects_conflicts_with_known_bindings() {
        let mut app_settings = get_default_settings();
        app_settings
            .transcription_presets
            .push(test_preset("preset_conflict"));
        let transcribe = app_settings
            .bindings
            .get("transcribe")
            .unwrap()
            .current_binding
            .clone();

        assert!(validate_binding_conflict(&app_settings, "preset_conflict", &transcribe).is_err());

        app_settings
            .transcription_presets
            .push(test_preset("preset_other"));
        app_settings.bindings.insert(
            "preset_other".to_string(),
            ShortcutBinding {
                id: "preset_other".to_string(),
                name: "Other preset".to_string(),
                description: "Other preset".to_string(),
                default_binding: "ctrl+alt+9".to_string(),
                current_binding: "ctrl+alt+9".to_string(),
            },
        );
        assert!(validate_binding_conflict(&app_settings, "preset_conflict", "ctrl+alt+9").is_err());
    }

    #[test]
    fn backend_switch_preparation_uses_dynamic_reset_binding_without_persisting_early() {
        let mut app_settings = get_default_settings();
        let mut preset = test_preset("preset_modifier_only");
        preset.enabled = true;
        app_settings.transcription_presets.push(preset);
        app_settings.bindings.insert(
            "preset_modifier_only".to_string(),
            ShortcutBinding {
                id: "preset_modifier_only".to_string(),
                name: "Modifier only".to_string(),
                description: "Modifier only".to_string(),
                default_binding: "ctrl+space".to_string(),
                current_binding: "shift".to_string(),
            },
        );

        let (target, reset) =
            prepare_settings_for_implementation(&app_settings, KeyboardImplementation::Tauri)
                .unwrap();
        assert_eq!(reset, vec!["preset_modifier_only".to_string()]);
        assert_eq!(
            app_settings.bindings["preset_modifier_only"].current_binding,
            "shift"
        );
        assert_eq!(
            target.bindings["preset_modifier_only"].current_binding,
            "ctrl+space"
        );
    }

    #[test]
    fn dynamic_preset_creation_stops_at_ten_and_does_not_create_a_binding() {
        let mut settings = get_default_settings();

        for index in 0..settings::MAX_TRANSCRIPTION_PRESETS {
            let preset = create_transcription_preset_in_settings(
                &mut settings,
                format!("preset_test_{index}"),
            )
            .unwrap();
            assert!(!preset.enabled);
            assert!(!settings.bindings.contains_key(&preset.id));
        }

        assert!(create_transcription_preset_in_settings(
            &mut settings,
            "preset_over_limit".to_string()
        )
        .is_err());
    }

    #[test]
    fn deleting_prompt_turns_off_affected_preset_post_processing() {
        let mut settings = get_default_settings();
        let mut first = test_preset("preset_a");
        first.post_process = true;
        first.post_process_prompt_id = Some("prompt-a".to_string());
        let mut second = test_preset("preset_b");
        second.post_process = true;
        second.post_process_prompt_id = Some("prompt-b".to_string());
        settings.transcription_presets = vec![first, second];

        reconcile_presets_for_deleted_prompt(&mut settings, "prompt-a");

        assert!(!settings.transcription_presets[0].post_process);
        assert_eq!(
            settings.transcription_presets[0].post_process_prompt_id,
            None
        );
        assert!(settings.transcription_presets[1].post_process);
        assert_eq!(
            settings.transcription_presets[1]
                .post_process_prompt_id
                .as_deref(),
            Some("prompt-b")
        );
    }
}
