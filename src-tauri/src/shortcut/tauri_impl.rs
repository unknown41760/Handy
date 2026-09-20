//! Tauri global-shortcut implementation
//!
//! This module provides shortcut functionality using Tauri's built-in
//! global-shortcut plugin.

use log::{debug, error, warn};
use tauri::AppHandle;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

#[cfg(not(target_os = "linux"))]
use crate::settings::get_settings;
use crate::settings::{self, ShortcutBinding};

use super::handler::handle_shortcut_event;

/// Initialize shortcuts using Tauri's global-shortcut plugin. Startup is
/// transactional: a partial native registration set is rolled back and the
/// caller is told initialization did not complete.
pub fn init_shortcuts(app: &AppHandle) -> Result<(), String> {
    let user_settings = settings::load_or_create_app_settings(app);
    let mut registered: Vec<ShortcutBinding> = Vec::new();

    for (id, binding) in &user_settings.bindings {
        if id == "cancel" {
            continue;
        }
        if !settings::is_known_shortcut_binding(&user_settings, id) {
            continue;
        }
        if !settings::is_optional_shortcut_enabled(&user_settings, id) {
            continue;
        }

        if let Err(error) = register_shortcut(app, binding.clone()) {
            let mut rollback_failures = Vec::new();
            for registered_binding in registered.iter().rev() {
                if let Err(rollback_error) = unregister_shortcut(app, registered_binding.clone()) {
                    rollback_failures
                        .push(format!("{}: {}", registered_binding.id, rollback_error));
                }
            }

            let mut message = format!(
                "Failed to register Tauri shortcut {} during init: {}",
                id, error
            );
            if !rollback_failures.is_empty() {
                message.push_str(&format!(
                    "; rollback incomplete: {}",
                    rollback_failures.join("; ")
                ));
            }
            return Err(message);
        }
        registered.push(binding.clone());
    }

    Ok(())
}

/// Validate a shortcut string for the Tauri global-shortcut implementation.
/// Tauri requires at least one non-modifier key and doesn't support the fn key.
pub fn validate_shortcut(raw: &str) -> Result<(), String> {
    if raw.trim().is_empty() {
        return Err("Shortcut cannot be empty".into());
    }

    let modifiers = [
        "ctrl", "control", "shift", "alt", "option", "meta", "command", "cmd", "super", "win",
        "windows",
    ];

    // Check for fn key which Tauri doesn't support
    let parts: Vec<String> = raw.split('+').map(|p| p.trim().to_lowercase()).collect();
    for part in &parts {
        if part == "fn" || part == "function" {
            return Err("The 'fn' key is not supported by Tauri global shortcuts".into());
        }
    }

    // Check for at least one non-modifier key
    let has_non_modifier = parts.iter().any(|part| !modifiers.contains(&part.as_str()));

    if has_non_modifier {
        Ok(())
    } else {
        Err("Tauri shortcuts must include a main key (letter, number, F-key, etc.) in addition to modifiers".into())
    }
}

/// Register a shortcut using Tauri's global-shortcut plugin
pub fn register_shortcut(app: &AppHandle, binding: ShortcutBinding) -> Result<(), String> {
    // Validate for Tauri requirements
    if let Err(e) = validate_shortcut(&binding.current_binding) {
        warn!(
            "register_tauri_shortcut validation error for binding '{}': {}",
            binding.current_binding, e
        );
        return Err(e);
    }

    // Parse shortcut and return error if it fails
    let shortcut = match binding.current_binding.parse::<Shortcut>() {
        Ok(s) => s,
        Err(e) => {
            let error_msg = format!(
                "Failed to parse shortcut '{}': {}",
                binding.current_binding, e
            );
            error!("register_tauri_shortcut parse error: {}", error_msg);
            return Err(error_msg);
        }
    };

    // Prevent duplicate registrations that would silently shadow one another
    if app.global_shortcut().is_registered(shortcut) {
        let error_msg = format!("Shortcut '{}' is already in use", binding.current_binding);
        warn!("register_tauri_shortcut duplicate error: {}", error_msg);
        return Err(error_msg);
    }

    // Clone binding.id for use in the closure
    let binding_id_for_closure = binding.id.clone();

    app.global_shortcut()
        .on_shortcut(shortcut, move |app_handle, scut, event| {
            if scut == &shortcut {
                let shortcut_string = scut.into_string();
                let is_pressed = event.state == ShortcutState::Pressed;
                // Mirrors the handy-keys event log line; the distinct prefix
                // makes it possible to tell which backend fired a shortcut
                // (e.g. when diagnosing the Secure Input fallback)
                debug!(
                    "tauri global-shortcut event: binding={}, shortcut={}, state={:?}",
                    binding_id_for_closure, shortcut_string, event.state
                );
                handle_shortcut_event(
                    app_handle,
                    &binding_id_for_closure,
                    &shortcut_string,
                    is_pressed,
                );
            }
        })
        .map_err(|e| {
            let error_msg = format!(
                "Couldn't register shortcut '{}': {}",
                binding.current_binding, e
            );
            error!("register_tauri_shortcut registration error: {}", error_msg);
            error_msg
        })?;

    Ok(())
}

/// Ensure a shortcut is registered, treating an already-present app
/// registration as success. This is used when restoring bindings after
/// shortcut capture, where the just-committed binding may already be live.
pub fn ensure_shortcut_registered(app: &AppHandle, binding: ShortcutBinding) -> Result<(), String> {
    let shortcut = binding.current_binding.parse::<Shortcut>().map_err(|e| {
        format!(
            "Failed to parse shortcut '{}' while restoring: {}",
            binding.current_binding, e
        )
    })?;
    if app.global_shortcut().is_registered(shortcut) {
        return Ok(());
    }
    register_shortcut(app, binding)
}

/// Unregister a shortcut from Tauri's global-shortcut plugin
pub fn unregister_shortcut(app: &AppHandle, binding: ShortcutBinding) -> Result<(), String> {
    let shortcut = match binding.current_binding.parse::<Shortcut>() {
        Ok(s) => s,
        Err(e) => {
            let error_msg = format!(
                "Failed to parse shortcut '{}' for unregistration: {}",
                binding.current_binding, e
            );
            error!("unregister_tauri_shortcut parse error: {}", error_msg);
            return Err(error_msg);
        }
    };

    // Shortcut capture suspends registrations before `change_binding` runs.
    // Treat an already-absent app registration as successfully unregistered,
    // while still propagating a real native teardown failure for a binding
    // that is currently registered.
    if !app.global_shortcut().is_registered(shortcut) {
        return Ok(());
    }

    app.global_shortcut().unregister(shortcut).map_err(|e| {
        let error_msg = format!(
            "Failed to unregister shortcut '{}': {}",
            binding.current_binding, e
        );
        error!("unregister_tauri_shortcut error: {}", error_msg);
        error_msg
    })?;

    Ok(())
}

/// Register the cancel shortcut (called when recording starts)
pub fn register_cancel_shortcut(app: &AppHandle) {
    // Cancel shortcut is disabled on Linux due to instability with dynamic shortcut registration
    #[cfg(target_os = "linux")]
    {
        let _ = app;
        return;
    }

    #[cfg(not(target_os = "linux"))]
    {
        let app_clone = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Some(cancel_binding) = get_settings(&app_clone).bindings.get("cancel").cloned() {
                if let Err(e) = register_shortcut(&app_clone, cancel_binding) {
                    error!("Failed to register cancel shortcut: {}", e);
                }
            }
        });
    }
}

/// Unregister the cancel shortcut (called when recording stops)
pub fn unregister_cancel_shortcut(app: &AppHandle) {
    // Cancel shortcut is disabled on Linux due to instability with dynamic shortcut registration
    #[cfg(target_os = "linux")]
    {
        let _ = app;
        return;
    }

    #[cfg(not(target_os = "linux"))]
    {
        let app_clone = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Some(cancel_binding) = get_settings(&app_clone).bindings.get("cancel").cloned() {
                // We ignore errors here as it might already be unregistered
                let _ = unregister_shortcut(&app_clone, cancel_binding);
            }
        });
    }
}
