use crate::input;
use crate::managers::transcription::TranscriptionManager;
use crate::settings::{
    self, active_transcription_operation, replace_active_transcription_operation,
    resolve_active_transcription_operation, resolve_selectable_transcription_preset,
    QuickSelectorPosition,
};
use handy_keys::Key;
#[cfg(not(target_os = "windows"))]
use handy_keys::KeyboardListener;
use serde::Serialize;
use specta::Type;
use std::f64::consts::TAU;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, Position, WebviewUrl, WebviewWindowBuilder};

pub const QUICK_SELECTOR_BINDING_ID: &str = "quick_preset_selector";
const WINDOW_LABEL: &str = "quick_preset_selector";
const WINDOW_SIZE: f64 = 315.0;
const SELECTION_DEAD_ZONE: f64 = 33.0;

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
    initial_cursor: (i32, i32),
    mouse_ready: bool,
    mouse_radius: f64,
    placement: QuickSelectorPosition,
    keyboard_engaged: bool,
    generation: u64,
    keyboard_x: i8,
    keyboard_y: i8,
}

#[derive(Default)]
pub struct QuickPresetSelectorState {
    session: Mutex<Option<SelectorSession>>,
    generation: AtomicU64,
    #[cfg(not(target_os = "windows"))]
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

#[tauri::command]
#[specta::specta]
pub fn change_quick_selector_position_setting(app: AppHandle, position: QuickSelectorPosition) {
    let mut app_settings = settings::get_settings(&app);
    app_settings.quick_selector_position = position;
    settings::write_settings(&app, app_settings);
}

fn slot_for_cursor(session: SelectorSession, cursor: (i32, i32)) -> Option<u8> {
    let dx = f64::from(cursor.0 - session.center_x);
    let dy = f64::from(cursor.1 - session.center_y);
    let distance = dx.hypot(dy);
    let dead_zone = SELECTION_DEAD_ZONE * session.mouse_radius / 150.0;
    if distance < dead_zone || distance > session.mouse_radius {
        return None;
    }

    if session.placement == QuickSelectorPosition::Bottom
        && (f64::from(cursor.0 - session.initial_cursor.0)
            .hypot(f64::from(cursor.1 - session.initial_cursor.1)))
            < 6.0
    {
        return None;
    }

    // atan2(dx, -dy) makes zero point up and positive angles move clockwise.
    let angle = dx.atan2(-dy).rem_euclid(TAU);
    Some(((angle / (TAU / 8.0)).round() as u8 % 8) + 1)
}

fn slot_for_release(session: SelectorSession, cursor: Option<(i32, i32)>) -> Option<u8> {
    if session.keyboard_engaged {
        slot_for_keyboard_position(session.keyboard_x, session.keyboard_y)
    } else if session.mouse_ready {
        cursor.and_then(|cursor| slot_for_cursor(session, cursor))
    } else {
        None
    }
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

fn bottom_center_for_cursor(
    window: &tauri::WebviewWindow,
    cursor: (i32, i32),
) -> Option<((i32, i32), f64)> {
    let monitors = window.available_monitors().ok()?;
    let monitor = monitors
        .iter()
        .find(|monitor| {
            let position = monitor.position();
            let size = monitor.size();
            cursor.0 >= position.x
                && cursor.0 < position.x + size.width as i32
                && cursor.1 >= position.y
                && cursor.1 < position.y + size.height as i32
        })
        .or_else(|| monitors.first())?;
    let work = monitor.work_area();
    let scale = monitor.scale_factor();
    let half = (WINDOW_SIZE * scale / 2.0).round() as i32;
    let margin = (8.0 * scale).round() as i32;
    Some((
        (
            work.position.x + work.size.width as i32 / 2,
            work.position.y + work.size.height as i32 - half - margin,
        ),
        scale,
    ))
}

fn position_selector(window: &tauri::WebviewWindow, center: (i32, i32), scale: f64) {
    let half = (WINDOW_SIZE * scale / 2.0).round() as i32;
    let _ = window.set_position(Position::Physical(tauri::PhysicalPosition::new(
        center.0 - half,
        center.1 - half,
    )));
}

fn position_current_session(
    app: &AppHandle,
    window: &tauri::WebviewWindow,
    generation: u64,
) -> Option<(bool, Option<u8>)> {
    let state = app.try_state::<QuickPresetSelectorState>()?;
    let mut guard = state
        .session
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let session = guard.as_mut()?;
    if session.generation != generation {
        return None;
    }
    let bottom = session.placement == QuickSelectorPosition::Bottom
        || (session.placement == QuickSelectorPosition::Auto && session.keyboard_engaged);
    let (center, scale) = if bottom {
        bottom_center_for_cursor(window, session.initial_cursor)
            .unwrap_or((session.initial_cursor, window.scale_factor().unwrap_or(1.0)))
    } else {
        (session.initial_cursor, window.scale_factor().unwrap_or(1.0))
    };
    position_selector(window, center, scale);
    session.center_x = center.0;
    session.center_y = center.1;
    session.mouse_radius = 150.0 * scale;
    session.mouse_ready = true;
    Some((
        session.keyboard_engaged,
        slot_for_keyboard_position(session.keyboard_x, session.keyboard_y),
    ))
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

fn session_is_current(app: &AppHandle, generation: u64) -> bool {
    app.try_state::<QuickPresetSelectorState>()
        .is_some_and(|state| {
            state
                .session
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_ref()
                .is_some_and(|session| session.generation == generation)
        })
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

/// The selector needs key presses but no mouse-button hotkeys. On Windows,
/// handy-keys' KeyboardListener also installs a system-wide WH_MOUSE_LL hook;
/// polling these few keys while the selector is open avoids another global
/// mouse hook in the input path.
#[cfg(target_os = "windows")]
const WINDOWS_SELECTOR_KEYS: [(i32, Key); 14] = [
    (0x31, Key::Num1),
    (0x32, Key::Num2),
    (0x33, Key::Num3),
    (0x34, Key::Num4),
    (0x35, Key::Num5),
    (0x36, Key::Num6),
    (0x37, Key::Num7),
    (0x38, Key::Num8),
    (0x26, Key::UpArrow),
    (0x28, Key::DownArrow),
    (0x25, Key::LeftArrow),
    (0x27, Key::RightArrow),
    (0x0D, Key::Return),
    (0x1B, Key::Escape),
];

#[cfg(target_os = "windows")]
struct WindowsSelectorKeys {
    held: u16,
    pending: u16,
}

#[cfg(target_os = "windows")]
impl WindowsSelectorKeys {
    fn sample() -> u16 {
        use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

        WINDOWS_SELECTOR_KEYS
            .iter()
            .enumerate()
            .fold(0, |held, (index, (vk, _))| {
                if unsafe { GetAsyncKeyState(*vk) } < 0 {
                    held | (1 << index)
                } else {
                    held
                }
            })
    }

    fn new() -> Self {
        Self {
            held: Self::sample(),
            pending: 0,
        }
    }

    fn next_key(&mut self) -> Option<Key> {
        self.advance(Self::sample())
    }

    fn advance(&mut self, now: u16) -> Option<Key> {
        self.pending |= now & !self.held;
        self.held = now;
        if self.pending == 0 {
            return None;
        }
        let index = self.pending.trailing_zeros() as usize;
        self.pending &= !(1 << index);
        Some(WINDOWS_SELECTOR_KEYS[index].1)
    }
}

fn stop_keyboard_selection(state: &QuickPresetSelectorState) {
    state.keyboard_running.store(false, Ordering::SeqCst);
    #[cfg(not(target_os = "windows"))]
    {
        *state
            .keyboard_listener
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }
}

fn start_keyboard_selection(app: &AppHandle) {
    let Some(state) = app.try_state::<QuickPresetSelectorState>() else {
        return;
    };
    stop_keyboard_selection(&state);
    #[cfg(not(target_os = "windows"))]
    {
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
    }
    #[cfg(target_os = "windows")]
    let mut polled_keys = WindowsSelectorKeys::new();
    state.keyboard_running.store(true, Ordering::SeqCst);

    let handle = app.clone();
    let running = Arc::clone(&state.keyboard_running);
    let generation = state.generation.load(Ordering::SeqCst);
    std::thread::spawn(move || {
        while running.load(Ordering::SeqCst)
            && handle
                .try_state::<QuickPresetSelectorState>()
                .is_some_and(|state| state.generation.load(Ordering::SeqCst) == generation)
        {
            #[cfg(target_os = "windows")]
            let key = polled_keys.next_key();
            #[cfg(not(target_os = "windows"))]
            let key = handle
                .try_state::<QuickPresetSelectorState>()
                .and_then(|state| {
                    state
                        .keyboard_listener
                        .lock()
                        .ok()
                        .and_then(|listener| listener.as_ref()?.try_recv())
                })
                .and_then(|event| event.is_key_down.then_some(event.key).flatten());
            if let Some(key) = key {
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
                                let mut session = state
                                    .session
                                    .lock()
                                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                                if session.as_ref()?.generation != generation {
                                    return None;
                                }
                                session.take()
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
                                if session.as_ref()?.generation != generation {
                                    return None;
                                }
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
                    let cancelled =
                        handle
                            .try_state::<QuickPresetSelectorState>()
                            .is_some_and(|state| {
                                let mut session = state
                                    .session
                                    .lock()
                                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                                if session
                                    .as_ref()
                                    .is_some_and(|session| session.generation == generation)
                                {
                                    state.generation.fetch_add(1, Ordering::SeqCst);
                                    *session = None;
                                    true
                                } else {
                                    false
                                }
                            });
                    if cancelled {
                        running.store(false, Ordering::SeqCst);
                        hide_now(&handle);
                        break;
                    }
                } else if matches!(
                    key,
                    Key::UpArrow | Key::DownArrow | Key::LeftArrow | Key::RightArrow
                ) {
                    let navigation =
                        handle
                            .try_state::<QuickPresetSelectorState>()
                            .and_then(|state| {
                                let mut session = state
                                    .session
                                    .lock()
                                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                                if session.as_ref()?.generation != generation {
                                    return None;
                                }
                                let session = session.as_mut()?;
                                let first_arrow = !session.keyboard_engaged;
                                session.keyboard_engaged = true;
                                let switch_to_bottom =
                                    first_arrow && session.placement == QuickSelectorPosition::Auto;
                                let highlighted_slot = move_keyboard_position(session, key);
                                Some((highlighted_slot, first_arrow, switch_to_bottom))
                            });
                    if let Some((highlighted_slot, first_arrow, switch_to_bottom)) = navigation {
                        if first_arrow {
                            let _ = handle.emit_to(WINDOW_LABEL, "quick-preset-keyboard-mode", ());
                        }
                        if switch_to_bottom {
                            let main_handle = handle.clone();
                            let _ = handle.run_on_main_thread(move || {
                                if let Some(window) = main_handle.get_webview_window(WINDOW_LABEL) {
                                    let _ =
                                        position_current_session(&main_handle, &window, generation);
                                }
                            });
                        }
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
            if state.generation.load(Ordering::SeqCst) == generation {
                stop_keyboard_selection(&state);
            }
        }
    });
}

pub fn open(app: &AppHandle) {
    let opened_at = Instant::now();
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
        initial_cursor: cursor,
        mouse_ready: false,
        mouse_radius: 150.0,
        placement: app_settings.quick_selector_position,
        keyboard_engaged: false,
        generation,
        keyboard_x: 0,
        keyboard_y: 0,
    });

    let handle = app.clone();
    let payload = selector_payload(&app_settings);
    let _ = app.run_on_main_thread(move || {
        if !session_is_current(&handle, generation) {
            return;
        }
        match get_or_create_window(&handle) {
            Ok(window) => {
                // WebView creation can take longer than a tap on slower machines.
                // A released or superseded selector must never be shown afterward.
                if !session_is_current(&handle, generation) {
                    return;
                }
                let navigation = position_current_session(&handle, &window, generation);
                let _ = window.emit("show-quick-preset-selector", payload);
                let _ = window.set_always_on_top(true);
                let _ = window.show();
                if !session_is_current(&handle, generation) {
                    let _ = window.hide();
                    return;
                }
                #[cfg(target_os = "windows")]
                force_selector_topmost(&window);
                if let Some((true, slot)) = navigation {
                    let _ = window.emit("quick-preset-keyboard-mode", ());
                    let _ = window.emit("quick-preset-highlighted", slot);
                }
                let elapsed = opened_at.elapsed();
                if elapsed > Duration::from_millis(150) {
                    log::warn!(
                        "Quick preset selector took {}ms to show",
                        elapsed.as_millis()
                    );
                }
            }
            Err(error) => log::error!("{error}"),
        }
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
        std::thread::sleep(Duration::from_millis(440));
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
    let slot = slot_for_release(session, input::get_cursor_position(app));
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
        slot_for_keyboard_position, slot_for_release, SelectorSession,
    };
    use crate::settings::{get_default_settings, TranscriptionPreset};
    use handy_keys::Key;

    #[cfg(target_os = "windows")]
    use super::WindowsSelectorKeys;

    fn session() -> SelectorSession {
        SelectorSession {
            center_x: 100,
            center_y: 100,
            initial_cursor: (100, 100),
            mouse_ready: true,
            mouse_radius: 150.0,
            placement: crate::settings::QuickSelectorPosition::Mouse,
            keyboard_engaged: false,
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
        assert_eq!(slot_for_cursor(session, (100, 68)), None);
        assert_eq!(slot_for_cursor(session, (100, 66)), Some(1));
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
    fn keyboard_center_cancels_instead_of_using_mouse_position() {
        let mut session = session();
        session.keyboard_engaged = true;
        assert_eq!(slot_for_release(session, Some((100, 0))), None);
    }

    #[test]
    fn bottom_placement_requires_mouse_movement_into_flower() {
        let mut session = session();
        session.placement = crate::settings::QuickSelectorPosition::Bottom;
        session.center_x = 500;
        session.center_y = 500;
        session.initial_cursor = (500, 410);
        assert_eq!(slot_for_release(session, Some((500, 410))), None);
        assert_eq!(slot_for_release(session, Some((500, 400))), Some(1));
        assert_eq!(slot_for_release(session, Some((500, 100))), None);
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

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_key_poll_reports_new_presses_once() {
        let mut keys = WindowsSelectorKeys {
            held: 1, // Num1 was already down when the selector opened.
            pending: 0,
        };
        assert_eq!(keys.advance(1), None);
        assert_eq!(keys.advance(1 | (1 << 8) | (1 << 9)), Some(Key::UpArrow));
        assert_eq!(keys.advance(1 | (1 << 8) | (1 << 9)), Some(Key::DownArrow));
        assert_eq!(keys.advance(1 | (1 << 8) | (1 << 9)), None);
        assert_eq!(keys.advance(1), None);
        assert_eq!(keys.advance(1 | (1 << 8)), Some(Key::UpArrow));
    }
}
