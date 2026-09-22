use crate::input;
use crate::managers::transcription::TranscriptionManager;
use crate::settings::{
    self, active_transcription_operation, replace_active_transcription_operation,
    resolve_active_transcription_operation, resolve_selectable_transcription_preset,
};
use handy_keys::{Key, KeyboardListener};
use serde::Serialize;
use specta::Type;
use std::f64::consts::TAU;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, Position, WebviewUrl, WebviewWindowBuilder};

pub const QUICK_SELECTOR_BINDING_ID: &str = "quick_preset_selector";
const WINDOW_LABEL: &str = "quick_preset_selector";
const WINDOW_SIZE: f64 = 420.0;
const SELECTION_DEAD_ZONE: f64 = 58.0;

#[derive(Clone, Debug, Serialize, Type)]
pub struct EffectiveTranscriptionTarget {
    pub preset_id: Option<String>,
    pub preset_name: Option<String>,
    pub model_id: String,
    pub recording: bool,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct QuickPresetSlot {
    pub slot: u8,
    pub preset_id: Option<String>,
    pub name: String,
    pub active: bool,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct QuickPresetSelectorPayload {
    pub slots: Vec<QuickPresetSlot>,
    pub active_preset_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct ActivePresetSelection {
    pub preset_id: Option<String>,
    pub preset_name: String,
    pub model_id: String,
}

#[derive(Clone, Copy)]
struct SelectorSession {
    center_x: i32,
    center_y: i32,
    generation: u64,
    keyboard_x: i8,
    keyboard_y: i8,
}

#[derive(Default)]
pub struct QuickPresetSelectorState {
    session: Mutex<Option<SelectorSession>>,
    generation: AtomicU64,
    keyboard_listener: Mutex<Option<KeyboardListener>>,
    keyboard_running: Arc<AtomicBool>,
}

fn selector_payload(settings: &settings::AppSettings) -> QuickPresetSelectorPayload {
    let mut slots = Vec::with_capacity(8);
    slots.push(QuickPresetSlot {
        slot: 1,
        preset_id: None,
        name: "Default".to_string(),
        active: settings.active_transcription_preset_id.is_none(),
    });
    for slot in 2..=8 {
        let preset = settings
            .transcription_presets
            .iter()
            .find(|preset| preset.quick_slot == Some(slot));
        slots.push(QuickPresetSlot {
            slot,
            preset_id: preset.map(|preset| preset.id.clone()),
            name: preset.map(|preset| preset.name.clone()).unwrap_or_default(),
            active: preset.is_some_and(|preset| {
                settings.active_transcription_preset_id.as_deref() == Some(preset.id.as_str())
            }),
        });
    }
    QuickPresetSelectorPayload {
        slots,
        active_preset_id: settings.active_transcription_preset_id.clone(),
    }
}

pub fn effective_transcription_target(app: &AppHandle) -> EffectiveTranscriptionTarget {
    if let Some(operation) = active_transcription_operation(app) {
        return EffectiveTranscriptionTarget {
            preset_id: operation.preset_id,
            preset_name: operation.preset_name,
            model_id: operation.settings.selected_model,
            recording: true,
        };
    }

    let operation = resolve_active_transcription_operation(settings::get_settings(app), false);
    EffectiveTranscriptionTarget {
        preset_id: operation.preset_id,
        preset_name: operation.preset_name,
        model_id: operation.settings.selected_model,
        recording: false,
    }
}

pub fn emit_effective_transcription_target(app: &AppHandle) {
    let _ = app.emit(
        "effective-transcription-target-changed",
        effective_transcription_target(app),
    );
}

fn active_selection_from_operation(
    operation: &settings::TranscriptionOperationConfig,
) -> ActivePresetSelection {
    ActivePresetSelection {
        preset_id: operation.preset_id.clone(),
        preset_name: operation
            .preset_name
            .clone()
            .unwrap_or_else(|| "Default".to_string()),
        model_id: operation.settings.selected_model.clone(),
    }
}

fn set_active_preset_inner(
    app: &AppHandle,
    preset_id: Option<String>,
) -> Result<ActivePresetSelection, String> {
    let mut app_settings = settings::get_settings(app);
    let operation = match preset_id.as_deref() {
        Some(id) => resolve_selectable_transcription_preset(&app_settings, id)?,
        None => {
            app_settings.active_transcription_preset_id = None;
            resolve_active_transcription_operation(app_settings.clone(), false)
        }
    };

    app_settings.active_transcription_preset_id = operation.preset_id.clone();
    let recording_changed = replace_active_transcription_operation(app, operation.clone());
    settings::write_settings(app, app_settings);
    let transcription_manager = app.state::<Arc<TranscriptionManager>>();
    if recording_changed {
        // A stream was configured from the previous selection. Never accept its
        // text for a new final preset; the full PCM remains in AudioRecordingManager
        // and will be batch-transcribed after stop.
        transcription_manager.cancel_stream();
    }

    let selection = active_selection_from_operation(&operation);
    let _ = app.emit("active-preset-changed", selection.clone());
    let _ = app.emit(
        "settings-changed",
        serde_json::json!({
            "setting": "active_transcription_preset_id",
            "value": selection.preset_id
        }),
    );
    emit_effective_transcription_target(app);

    if !operation.settings.selected_model.trim().is_empty()
        && (recording_changed
            || operation.settings.model_unload_timeout != settings::ModelUnloadTimeout::Immediately)
    {
        transcription_manager.initiate_model_load_for(&operation.settings.selected_model);
    }
    Ok(selection)
}

#[tauri::command]
#[specta::specta]
pub fn set_active_transcription_preset(
    app: AppHandle,
    preset_id: Option<String>,
) -> Result<ActivePresetSelection, String> {
    set_active_preset_inner(&app, preset_id)
}

#[tauri::command]
#[specta::specta]
pub fn get_effective_transcription_target(app: AppHandle) -> EffectiveTranscriptionTarget {
    effective_transcription_target(&app)
}

#[tauri::command]
#[specta::specta]
pub fn get_quick_preset_selector_payload(app: AppHandle) -> QuickPresetSelectorPayload {
    selector_payload(&settings::get_settings(&app))
}

fn slot_for_cursor(session: SelectorSession, cursor: (i32, i32)) -> Option<u8> {
    let dx = f64::from(cursor.0 - session.center_x);
    let dy = f64::from(cursor.1 - session.center_y);
    if dx.hypot(dy) < SELECTION_DEAD_ZONE {
        return None;
    }

    // atan2(dx, -dy) makes zero point up and positive angles move clockwise.
    let angle = dx.atan2(-dy).rem_euclid(TAU);
    Some(((angle / (TAU / 8.0)).round() as u8 % 8) + 1)
}

fn slot_for_keyboard_position(x: i8, y: i8) -> Option<u8> {
    match (x, y) {
        (0, -1) => Some(1),
        (1, -1) => Some(2),
        (1, 0) => Some(3),
        (1, 1) => Some(4),
        (0, 1) => Some(5),
        (-1, 1) => Some(6),
        (-1, 0) => Some(7),
        (-1, -1) => Some(8),
        _ => None,
    }
}

fn move_keyboard_position(session: &mut SelectorSession, key: Key) -> Option<Option<u8>> {
    let previous = (session.keyboard_x, session.keyboard_y);
    match key {
        Key::UpArrow => session.keyboard_y = (session.keyboard_y - 1).max(-1),
        Key::DownArrow => session.keyboard_y = (session.keyboard_y + 1).min(1),
        Key::LeftArrow => session.keyboard_x = (session.keyboard_x - 1).max(-1),
        Key::RightArrow => session.keyboard_x = (session.keyboard_x + 1).min(1),
        _ => return None,
    }
    let position = (session.keyboard_x, session.keyboard_y);
    (position != previous).then(|| slot_for_keyboard_position(position.0, position.1))
}

fn preset_id_for_slot(settings: &settings::AppSettings, slot: u8) -> Option<Option<String>> {
    if slot == 1 {
        return Some(None);
    }
    settings
        .transcription_presets
        .iter()
        .find(|preset| preset.quick_slot == Some(slot))
        .map(|preset| Some(preset.id.clone()))
}

fn position_selector(window: &tauri::WebviewWindow, cursor: (i32, i32)) {
    #[cfg(target_os = "windows")]
    {
        let scale = window.scale_factor().unwrap_or(1.0);
        let half = (WINDOW_SIZE * scale / 2.0).round() as i32;
        let _ = window.set_position(Position::Physical(tauri::PhysicalPosition::new(
            cursor.0 - half,
            cursor.1 - half,
        )));
    }
    #[cfg(not(target_os = "windows"))]
    {
        let half = WINDOW_SIZE / 2.0;
        let _ = window.set_position(Position::Logical(tauri::LogicalPosition::new(
            f64::from(cursor.0) - half,
            f64::from(cursor.1) - half,
        )));
    }
}

fn get_or_create_window(app: &AppHandle) -> Result<tauri::WebviewWindow, String> {
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        return Ok(window);
    }

    let mut builder = WebviewWindowBuilder::new(
        app,
        WINDOW_LABEL,
        WebviewUrl::App("src/quick-preset-selector/index.html".into()),
    )
    .title("Quick Preset Selector")
    .inner_size(WINDOW_SIZE, WINDOW_SIZE)
    .resizable(false)
    .shadow(false)
    .maximizable(false)
    .minimizable(false)
    .closable(false)
    .decorations(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .transparent(true)
    .focusable(false)
    .focused(false)
    .accept_first_mouse(true)
    .visible(false);
    if let Some(data_dir) = crate::portable::data_dir() {
        builder = builder.data_directory(data_dir.join("webview"));
    }
    builder
        .build()
        .map_err(|error| format!("Failed to create quick preset selector: {error}"))
}

/// Reasserts the selector's native Windows Z-order without activating it.
/// Tauri's always-on-top flag can be displaced when another topmost window is
/// shown after this reusable window was created.
#[cfg(target_os = "windows")]
fn force_selector_topmost(window: &tauri::WebviewWindow) {
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
    };

    if let Ok(hwnd) = window.hwnd() {
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
    }
}

fn stop_keyboard_selection(state: &QuickPresetSelectorState) {
    state.keyboard_running.store(false, Ordering::SeqCst);
    *state
        .keyboard_listener
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

fn start_keyboard_selection(app: &AppHandle) {
    let Some(state) = app.try_state::<QuickPresetSelectorState>() else {
        return;
    };
    stop_keyboard_selection(&state);
    let listener = match KeyboardListener::new() {
        Ok(listener) => listener,
        Err(error) => {
            log::warn!("Quick preset keyboard selection is unavailable: {error}");
            return;
        }
    };
    *state
        .keyboard_listener
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(listener);
    state.keyboard_running.store(true, Ordering::SeqCst);

    let handle = app.clone();
    let running = Arc::clone(&state.keyboard_running);
    std::thread::spawn(move || {
        while running.load(Ordering::SeqCst) {
            let event = handle
                .try_state::<QuickPresetSelectorState>()
                .and_then(|state| {
                    state
                        .keyboard_listener
                        .lock()
                        .ok()
                        .and_then(|listener| listener.as_ref()?.try_recv())
                });
            if let Some(event) = event {
                if event.is_key_down {
                    let Some(key) = event.key else {
                        continue;
                    };
                    let number_slot = key
                        .to_string()
                        .parse::<u8>()
                        .ok()
                        .filter(|slot| (1..=8).contains(slot));
                    if let Some(slot) = number_slot {
                        let session =
                            handle
                                .try_state::<QuickPresetSelectorState>()
                                .and_then(|state| {
                                    state
                                        .session
                                        .lock()
                                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                                        .take()
                                });
                        if let Some(session) = session {
                            running.store(false, Ordering::SeqCst);
                            if let Err(error) = confirm_slot(&handle, slot, session.generation) {
                                log::error!(
                                    "Failed to select quick preset slot {slot} from keyboard: {error}"
                                );
                                hide_now(&handle);
                            }
                            break;
                        }
                    } else if key == Key::Return {
                        let session =
                            handle
                                .try_state::<QuickPresetSelectorState>()
                                .and_then(|state| {
                                    let mut session = state
                                        .session
                                        .lock()
                                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                                    let slot = slot_for_keyboard_position(
                                        session.as_ref()?.keyboard_x,
                                        session.as_ref()?.keyboard_y,
                                    )?;
                                    session.take().map(|session| (slot, session.generation))
                                });
                        if let Some((slot, generation)) = session {
                            running.store(false, Ordering::SeqCst);
                            if let Err(error) = confirm_slot(&handle, slot, generation) {
                                log::error!(
                                    "Failed to select quick preset slot {slot} from keyboard: {error}"
                                );
                                hide_now(&handle);
                            }
                            break;
                        }
                    } else if key == Key::Escape {
                        running.store(false, Ordering::SeqCst);
                        if let Some(state) = handle.try_state::<QuickPresetSelectorState>() {
                            state.generation.fetch_add(1, Ordering::SeqCst);
                            *state
                                .session
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
                        }
                        hide_now(&handle);
                        break;
                    } else {
                        let highlighted_slot = handle
                            .try_state::<QuickPresetSelectorState>()
                            .and_then(|state| {
                                let mut session = state
                                    .session
                                    .lock()
                                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                                move_keyboard_position(session.as_mut()?, key)
                            });
                        if let Some(slot) = highlighted_slot {
                            let _ = handle.emit_to(WINDOW_LABEL, "quick-preset-highlighted", slot);
                        }
                    }
                }
            } else {
                std::thread::sleep(Duration::from_millis(8));
            }
        }
        if let Some(state) = handle.try_state::<QuickPresetSelectorState>() {
            stop_keyboard_selection(&state);
        }
    });
}

pub fn open(app: &AppHandle) {
    let app_settings = settings::get_settings(app);
    if app_settings.transcription_presets.is_empty() {
        return;
    }
    let Some(cursor) = input::get_cursor_position(app) else {
        return;
    };
    let Some(state) = app.try_state::<QuickPresetSelectorState>() else {
        return;
    };
    let generation = state.generation.fetch_add(1, Ordering::SeqCst) + 1;
    *state
        .session
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(SelectorSession {
        center_x: cursor.0,
        center_y: cursor.1,
        generation,
        keyboard_x: 0,
        keyboard_y: 0,
    });

    let handle = app.clone();
    let payload = selector_payload(&app_settings);
    let _ = app.run_on_main_thread(move || match get_or_create_window(&handle) {
        Ok(window) => {
            position_selector(&window, cursor);
            let _ = window.emit("show-quick-preset-selector", payload);
            let _ = window.set_always_on_top(true);
            let _ = window.show();
            #[cfg(target_os = "windows")]
            force_selector_topmost(&window);
        }
        Err(error) => log::error!("{error}"),
    });
    start_keyboard_selection(app);
}

fn hide_now(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        let _ = window.hide();
    }
}

fn hide_after_confirmation(app: &AppHandle, generation: u64) {
    let handle = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(650));
        let should_hide = handle
            .try_state::<QuickPresetSelectorState>()
            .is_some_and(|state| state.generation.load(Ordering::SeqCst) == generation);
        if should_hide {
            let main_handle = handle.clone();
            let _ = handle.run_on_main_thread(move || hide_now(&main_handle));
        }
    });
}

fn confirm_slot(app: &AppHandle, slot: u8, generation: u64) -> Result<(), String> {
    let app_settings = settings::get_settings(app);
    let Some(preset_id) = preset_id_for_slot(&app_settings, slot) else {
        hide_now(app);
        return Ok(());
    };
    let selection = set_active_preset_inner(app, preset_id)?;
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        let _ = window.emit("quick-preset-confirmed", selection);
    }
    hide_after_confirmation(app, generation);
    Ok(())
}

pub fn release(app: &AppHandle) {
    let Some(state) = app.try_state::<QuickPresetSelectorState>() else {
        return;
    };
    stop_keyboard_selection(&state);
    let session = state
        .session
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    let Some(session) = session else {
        return;
    };
    let keyboard_slot = slot_for_keyboard_position(session.keyboard_x, session.keyboard_y);
    let slot = keyboard_slot.or_else(|| {
        input::get_cursor_position(app).and_then(|cursor| slot_for_cursor(session, cursor))
    });
    match slot {
        Some(slot) => {
            if let Err(error) = confirm_slot(app, slot, session.generation) {
                log::error!("Failed to select quick preset slot {slot}: {error}");
                hide_now(app);
            }
        }
        None => hide_now(app),
    }
}

#[tauri::command]
#[specta::specta]
pub fn select_quick_preset_slot(app: AppHandle, slot: u8) -> Result<(), String> {
    if !(1..=8).contains(&slot) {
        return Err(format!(
            "Quick preset slot must be between 1 and 8, got {slot}"
        ));
    }
    let generation = app
        .try_state::<QuickPresetSelectorState>()
        .map(|state| {
            stop_keyboard_selection(&state);
            state
                .session
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
                .map(|session| session.generation)
                .unwrap_or_else(|| state.generation.load(Ordering::SeqCst))
        })
        .unwrap_or_default();
    confirm_slot(&app, slot, generation)
}

#[tauri::command]
#[specta::specta]
pub fn close_quick_preset_selector(app: AppHandle) {
    if let Some(state) = app.try_state::<QuickPresetSelectorState>() {
        stop_keyboard_selection(&state);
        state.generation.fetch_add(1, Ordering::SeqCst);
        *state
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }
    hide_now(&app);
}

pub fn destroy(app: &AppHandle) {
    if let Some(state) = app.try_state::<QuickPresetSelectorState>() {
        stop_keyboard_selection(&state);
        state.generation.fetch_add(1, Ordering::SeqCst);
        *state
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }

    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(window) = handle.get_webview_window(WINDOW_LABEL) {
            if let Err(error) = window.destroy() {
                log::warn!("Failed to destroy unused quick preset selector window: {error}");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{
        move_keyboard_position, preset_id_for_slot, selector_payload, slot_for_cursor,
        slot_for_keyboard_position, SelectorSession,
    };
    use crate::settings::{get_default_settings, TranscriptionPreset};
    use handy_keys::Key;

    fn session() -> SelectorSession {
        SelectorSession {
            center_x: 100,
            center_y: 100,
            generation: 1,
            keyboard_x: 0,
            keyboard_y: 0,
        }
    }

    fn preset(id: &str, slot: Option<u8>) -> TranscriptionPreset {
        TranscriptionPreset {
            id: id.to_string(),
            name: id.to_string(),
            enabled: false,
            model_id: String::new(),
            language: "auto".to_string(),
            translate_to_english: false,
            post_process: false,
            post_process_prompt_id: None,
            quick_slot: slot,
        }
    }

    #[test]
    fn cursor_directions_map_to_fixed_clockwise_slots() {
        let session = session();
        assert_eq!(slot_for_cursor(session, (100, 0)), Some(1));
        assert_eq!(slot_for_cursor(session, (200, 100)), Some(3));
        assert_eq!(slot_for_cursor(session, (100, 200)), Some(5));
        assert_eq!(slot_for_cursor(session, (0, 100)), Some(7));
        assert_eq!(slot_for_cursor(session, (110, 110)), None);
        assert_eq!(slot_for_cursor(session, (100, 45)), None);
        assert_eq!(slot_for_cursor(session, (100, 40)), Some(1));
    }

    #[test]
    fn keyboard_navigation_starts_in_center_and_reaches_all_directions() {
        let mut session = session();
        assert_eq!(
            slot_for_keyboard_position(session.keyboard_x, session.keyboard_y),
            None
        );
        assert_eq!(
            move_keyboard_position(&mut session, Key::UpArrow),
            Some(Some(1))
        );
        assert_eq!(
            move_keyboard_position(&mut session, Key::RightArrow),
            Some(Some(2))
        );
        assert_eq!(
            move_keyboard_position(&mut session, Key::DownArrow),
            Some(Some(3))
        );
        assert_eq!(
            move_keyboard_position(&mut session, Key::DownArrow),
            Some(Some(4))
        );
        assert_eq!(
            move_keyboard_position(&mut session, Key::LeftArrow),
            Some(Some(5))
        );
        assert_eq!(
            move_keyboard_position(&mut session, Key::LeftArrow),
            Some(Some(6))
        );
        assert_eq!(
            move_keyboard_position(&mut session, Key::UpArrow),
            Some(Some(7))
        );
        assert_eq!(
            move_keyboard_position(&mut session, Key::UpArrow),
            Some(Some(8))
        );
        assert_eq!(
            move_keyboard_position(&mut session, Key::RightArrow),
            Some(Some(1))
        );
    }

    #[test]
    fn opposite_arrow_returns_keyboard_navigation_to_center() {
        let mut session = session();
        assert_eq!(
            move_keyboard_position(&mut session, Key::UpArrow),
            Some(Some(1))
        );
        assert_eq!(
            move_keyboard_position(&mut session, Key::DownArrow),
            Some(None)
        );
        assert_eq!(
            slot_for_keyboard_position(session.keyboard_x, session.keyboard_y),
            None
        );
    }

    #[test]
    fn selector_keeps_all_eight_slots_and_default_at_one() {
        let mut settings = get_default_settings();
        settings.transcription_presets = vec![preset("preset_two", Some(2))];
        let payload = selector_payload(&settings);
        assert_eq!(payload.slots.len(), 8);
        assert_eq!(payload.slots[0].slot, 1);
        assert_eq!(payload.slots[0].name, "Default");
        assert_eq!(payload.slots[1].preset_id.as_deref(), Some("preset_two"));
        assert!(payload.slots[2].preset_id.is_none());
        assert_eq!(preset_id_for_slot(&settings, 1), Some(None));
        assert_eq!(
            preset_id_for_slot(&settings, 2),
            Some(Some("preset_two".to_string()))
        );
    }
}
