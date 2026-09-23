//! Windows reliable paste.
//!
//! Publishes the transcript as a *delayed-render* clipboard format
//! (`SetClipboardData(CF_UNICODETEXT, NULL)`) owned by a hidden message-only
//! window. Windows sends the owner `WM_RENDERFORMAT` when a consumer actually
//! requests the data — that message is the read receipt. The previous
//! clipboard contents (snapshotted with full format fidelity) are restored
//! once receipts go quiet (see `paste_tx::evaluate`), guarded by the clipboard
//! owner window so we never clobber a newer user copy.
//!
//! Threading: clipboard ownership and delayed rendering are per-thread and
//! need a message pump, so the whole transaction lives on a dedicated worker
//! thread. The calling thread only sends the paste chord once the worker
//! signals the transcript is published, then returns; the wait, guarded
//! restore and auto-submit all finish on the worker.

use std::sync::{mpsc::Sender, Arc, Mutex, MutexGuard, Once};
use std::thread;
use std::time::{Duration, Instant};

use log::{error, info, warn};
use tauri::Manager;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    SetLastError, ERROR_SUCCESS, HANDLE, HGLOBAL, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM,
};

use super::{evaluate, send_chord, TxState, WaitDecision, RESTORE_TIMEOUT};
use crate::clipboard::send_return_key;
use crate::input::EnigoState;
use crate::settings::{AutoSubmitKey, ClipboardHandling, PasteMethod};
use windows::Win32::Foundation::GlobalFree;
use windows::Win32::Graphics::Gdi::{DeleteObject, HGDIOBJ};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData, GetClipboardOwner,
    OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};
use windows::Win32::System::Ole::{
    CF_BITMAP, CF_DSPBITMAP, CF_DSPENHMETAFILE, CF_DSPMETAFILEPICT, CF_DSPTEXT, CF_ENHMETAFILE,
    CF_OWNERDISPLAY, CF_PALETTE, CF_UNICODETEXT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CopyImage, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetWindowLongPtrW, KillTimer, PostQuitMessage, RegisterClassW, SetTimer, SetWindowLongPtrW,
    GDI_IMAGE_TYPE, GWLP_USERDATA, HWND_MESSAGE, IMAGE_FLAGS, MSG, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_DESTROYCLIPBOARD, WM_RENDERALLFORMATS, WM_RENDERFORMAT, WM_TIMER, WNDCLASSW,
};

const CLASS_NAME: PCWSTR = w!("HandyPasteTxWindow");
const TIMER_ID: usize = 1;
const TIMER_INTERVAL_MS: u32 = 25;
/// Skip clipboard formats larger than this when snapshotting.
const MAX_FORMAT_BYTES: usize = 64 * 1024 * 1024;

const IMAGE_BITMAP_TYPE: GDI_IMAGE_TYPE = GDI_IMAGE_TYPE(0);
const LR_CREATEDIBSECTION_FLAG: IMAGE_FLAGS = IMAGE_FLAGS(0x2000);

struct SavedFormat {
    format: u32,
    data: Vec<u8>,
}

pub(super) struct WinTxShared {
    state: Mutex<TxState>,
    text: String,
    snapshot: Mutex<Vec<SavedFormat>>,
    /// Copied HBITMAP (as raw usize), restored via SetClipboardData.
    saved_bitmap: Mutex<Option<usize>>,
    app_handle: tauri::AppHandle,
    auto_submit: bool,
    auto_submit_key: AutoSubmitKey,
    /// ClipboardHandling::CopyToClipboard — settle by leaving the transcript
    /// on the clipboard as plain text instead of restoring the snapshot.
    preserve_transcript: bool,
}

impl Drop for WinTxShared {
    fn drop(&mut self) {
        if let Ok(mut bitmap) = self.saved_bitmap.lock() {
            if let Some(raw) = bitmap.take() {
                unsafe {
                    let _ = DeleteObject(HGDIOBJ(raw as *mut _));
                }
            }
        }
    }
}

/// The transaction currently holding the clipboard, if any. A new paste waits
/// for it to settle before publishing another transcript.
static PENDING: Mutex<Option<Arc<WinTxShared>>> = Mutex::new(None);
/// Serialize the check for a pending transaction with publishing the next one.
/// Without this gate, two callers can both observe an empty PENDING slot and
/// race to replace the clipboard.
static PASTE_START: Mutex<()> = Mutex::new(());

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn shared_ptr(hwnd: HWND) -> *const WinTxShared {
    GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const WinTxShared
}

/// Sends the auto-submit Enter. Uses `try_lock` because the paste caller may
/// currently hold the enigo lock while waiting for this worker.
fn send_auto_submit(shared: &WinTxShared) {
    {
        let mut st = match shared.state.lock() {
            Ok(st) => st,
            Err(_) => return,
        };
        if st.auto_submit_sent {
            return;
        }
        st.auto_submit_sent = true;
    }
    if let Some(enigo_state) = shared.app_handle.try_state::<EnigoState>() {
        match enigo_state.0.try_lock() {
            Ok(mut enigo) => {
                let _ = send_return_key(&mut enigo, shared.auto_submit_key);
            }
            Err(_) => warn!("[reliable-paste] skipping auto-submit: input state busy"),
        }
    }
}

/// Renders the promised transcript into the clipboard, which must already be
/// open: the system opens it on our behalf for WM_RENDERFORMAT; every other
/// caller has to wrap this in OpenClipboard/CloseClipboard itself.
unsafe fn render_text(shared: &WinTxShared) -> bool {
    let wide_text: Vec<u16> = shared
        .text
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let Ok(hg) = GlobalAlloc(GMEM_MOVEABLE, wide_text.len() * 2) else {
        return false;
    };
    let ptr = GlobalLock(hg) as *mut u16;
    if ptr.is_null() {
        let _ = GlobalFree(Some(hg));
        return false;
    }
    std::ptr::copy_nonoverlapping(wide_text.as_ptr(), ptr, wide_text.len());
    let _ = GlobalUnlock(hg);
    if SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(hg.0))).is_err() {
        let _ = GlobalFree(Some(hg));
        return false;
    }
    true
}

unsafe extern "system" fn paste_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let shared = shared_ptr(hwnd);
    match msg {
        WM_RENDERFORMAT => {
            if !shared.is_null() {
                let shared = &*shared;
                if wparam.0 as u32 == CF_UNICODETEXT.0 as u32 && render_text(shared) {
                    if let Ok(mut st) = shared.state.lock() {
                        st.record_receipt(Instant::now());
                    }
                }
            }
            LRESULT(0)
        }
        WM_RENDERALLFORMATS => {
            // Sent when the window is destroyed while an unrendered promise is
            // still on the clipboard — not a consumer read, so no receipt.
            // Unlike WM_RENDERFORMAT the system does not open the clipboard on
            // our behalf here: open it and confirm we still own it first.
            if !shared.is_null() {
                let shared = &*shared;
                if OpenClipboard(Some(hwnd)).is_ok() {
                    if GetClipboardOwner()
                        .map(|owner| owner == hwnd)
                        .unwrap_or(false)
                    {
                        let _ = render_text(shared);
                    }
                    let _ = CloseClipboard();
                }
            }
            LRESULT(0)
        }
        WM_DESTROYCLIPBOARD => {
            if !shared.is_null() {
                if let Ok(mut st) = (&*shared).state.lock() {
                    st.ownership_lost = true;
                }
            }
            LRESULT(0)
        }
        WM_TIMER => {
            if !shared.is_null() {
                on_timer(hwnd, &*shared);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn ensure_window_class(hinstance: HINSTANCE) {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(paste_wnd_proc),
            hInstance: hinstance,
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        unsafe {
            RegisterClassW(&wc);
        }
    });
}

/// Keep consecutive dictations in order. Replacing the clipboard while the
/// prior target is still reading it can paste the wrong transcription.
pub(super) fn wait_for_previous() -> Result<MutexGuard<'static, ()>, String> {
    let start_guard = PASTE_START
        .lock()
        .map_err(|_| "Reliable paste start state is unavailable")?;
    let deadline = Instant::now() + RESTORE_TIMEOUT + Duration::from_secs(1);
    loop {
        let pending = PENDING
            .lock()
            .map_err(|_| "Reliable paste state is unavailable")?
            .is_some();
        if !pending {
            return Ok(start_guard);
        }
        if Instant::now() >= deadline {
            return Err("Previous clipboard paste has not finished".to_string());
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// Settle-time clipboard handling once we know we still own the clipboard:
/// restore the snapshot, or — for ClipboardHandling::CopyToClipboard — replace
/// the concealed promise with plain transcript text, so clipboard history and
/// managers record it and it survives this transaction's window going away.
unsafe fn settle_clipboard(hwnd: HWND, shared: &WinTxShared) -> bool {
    if !shared.preserve_transcript {
        return restore_snapshot(hwnd, shared);
    }
    if OpenClipboard(Some(hwnd)).is_err() {
        warn!("[reliable-paste] could not open clipboard to leave transcript");
        return false;
    }
    if EmptyClipboard().is_err() {
        let _ = CloseClipboard();
        warn!("[reliable-paste] could not replace clipboard with transcript");
        return false;
    }
    if !render_text(shared) {
        let _ = CloseClipboard();
        warn!("[reliable-paste] could not render transcript");
        return false;
    }
    if CloseClipboard().is_err() {
        warn!("[reliable-paste] could not close clipboard after transcript");
        return false;
    }
    info!("[reliable-paste] left transcript on clipboard as plain text");
    true
}

/// Restores the snapshotted clipboard contents. Safe to call from any thread.
unsafe fn restore_snapshot(hwnd: HWND, shared: &WinTxShared) -> bool {
    if OpenClipboard(Some(hwnd)).is_err() {
        warn!("[reliable-paste] could not open clipboard to restore");
        return false;
    }
    if EmptyClipboard().is_err() {
        let _ = CloseClipboard();
        warn!("[reliable-paste] could not empty clipboard to restore");
        return false;
    }
    if let Ok(formats) = shared.snapshot.lock() {
        for saved in formats.iter() {
            if saved.data.is_empty() {
                continue;
            }
            let Ok(hg) = GlobalAlloc(GMEM_MOVEABLE, saved.data.len()) else {
                continue;
            };
            let ptr = GlobalLock(hg) as *mut u8;
            if ptr.is_null() {
                let _ = GlobalFree(Some(hg));
                continue;
            }
            std::ptr::copy_nonoverlapping(saved.data.as_ptr(), ptr, saved.data.len());
            let _ = GlobalUnlock(hg);
            // SetClipboardData takes ownership of the handle on success.
            if SetClipboardData(saved.format, Some(HANDLE(hg.0))).is_err() {
                let _ = GlobalFree(Some(hg));
            }
        }
    }
    if let Ok(mut bitmap) = shared.saved_bitmap.lock() {
        if let Some(raw) = bitmap.take() {
            if SetClipboardData(CF_BITMAP.0 as u32, Some(HANDLE(raw as *mut _))).is_err() {
                let _ = DeleteObject(HGDIOBJ(raw as *mut _));
            }
        }
    }
    // Restoring the old text or image is an internal operation; do not add a
    // duplicate entry to Windows clipboard history or cloud sync.
    write_history_marker("ExcludeClipboardContentFromMonitorProcessing", 1);
    write_history_marker("CanIncludeInClipboardHistory", 0);
    write_history_marker("CanUploadToCloudClipboard", 0);
    if CloseClipboard().is_err() {
        warn!("[reliable-paste] could not close clipboard after restore");
        return false;
    }
    info!("[reliable-paste] restored previous clipboard");
    true
}

unsafe fn snapshot_clipboard(hwnd: HWND, shared: &WinTxShared) -> Result<(), String> {
    OpenClipboard(Some(hwnd)).map_err(|e| format!("OpenClipboard failed: {e}"))?;
    let mut formats = Vec::new();
    let mut format = 0u32;
    loop {
        format = EnumClipboardFormats(format);
        if format == 0 {
            break;
        }
        if format == CF_BITMAP.0 as u32 {
            // GDI object, not global memory: duplicate the handle instead.
            let handle = match GetClipboardData(CF_BITMAP.0 as u32) {
                Ok(handle) => handle,
                Err(e) => {
                    let _ = CloseClipboard();
                    return Err(format!("Could not read clipboard bitmap: {e}"));
                }
            };
            let copy = match CopyImage(handle, IMAGE_BITMAP_TYPE, 0, 0, LR_CREATEDIBSECTION_FLAG) {
                Ok(copy) => copy,
                Err(e) => {
                    let _ = CloseClipboard();
                    return Err(format!("Could not copy clipboard bitmap: {e}"));
                }
            };
            if let Ok(mut slot) = shared.saved_bitmap.lock() {
                *slot = Some(copy.0 as usize);
            } else {
                let _ = DeleteObject(HGDIOBJ(copy.0));
                let _ = CloseClipboard();
                return Err("Clipboard bitmap state is unavailable".to_string());
            }
            continue;
        }
        // Formats whose handles are not plain global memory cannot be
        // byte-copied; skipping them matches what the legacy path restored.
        if format == CF_ENHMETAFILE.0 as u32
            || format == CF_DSPENHMETAFILE.0 as u32
            || format == CF_DSPBITMAP.0 as u32
            || format == CF_DSPMETAFILEPICT.0 as u32
            || format == CF_DSPTEXT.0 as u32
            || format == CF_OWNERDISPLAY.0 as u32
            || format == CF_PALETTE.0 as u32
        {
            continue;
        }
        if let Ok(handle) = GetClipboardData(format) {
            let hg = HGLOBAL(handle.0);
            let size = GlobalSize(hg);
            if size == 0 || size > MAX_FORMAT_BYTES {
                continue;
            }
            let ptr = GlobalLock(hg) as *const u8;
            if ptr.is_null() {
                continue;
            }
            let data = std::slice::from_raw_parts(ptr, size).to_vec();
            let _ = GlobalUnlock(hg);
            formats.push(SavedFormat { format, data });
        }
    }
    let _ = CloseClipboard();
    let mut slot = shared
        .snapshot
        .lock()
        .map_err(|_| "Clipboard snapshot state is unavailable")?;
    *slot = formats;
    Ok(())
}

/// Publishes the transcript as a delayed-render promise plus clipboard
/// history / cloud / monitoring opt-out markers (the same formats Chrome uses
/// for Incognito copies).
unsafe fn publish(hwnd: HWND) -> Result<(), String> {
    OpenClipboard(Some(hwnd)).map_err(|e| format!("OpenClipboard failed: {e}"))?;
    let published = publish_formats();
    let closed = CloseClipboard();
    published?;
    closed.map_err(|e| format!("CloseClipboard failed: {e}"))
}

unsafe fn write_history_marker(name: &str, value: u32) {
    let name_wide = wide(name);
    let format = RegisterClipboardFormatW(PCWSTR(name_wide.as_ptr()));
    if format == 0 {
        return;
    }
    let Ok(hg) = GlobalAlloc(GMEM_MOVEABLE, std::mem::size_of::<u32>()) else {
        return;
    };
    let ptr = GlobalLock(hg) as *mut u32;
    if ptr.is_null() {
        let _ = GlobalFree(Some(hg));
        return;
    }
    *ptr = value;
    let _ = GlobalUnlock(hg);
    if SetClipboardData(format, Some(HANDLE(hg.0))).is_err() {
        let _ = GlobalFree(Some(hg));
    }
}

/// Publish formats while the clipboard is open; `publish` closes it even
/// if a format cannot be published.
unsafe fn publish_formats() -> Result<(), String> {
    EmptyClipboard().map_err(|e| format!("EmptyClipboard failed: {e}"))?;

    write_history_marker("ExcludeClipboardContentFromMonitorProcessing", 1);
    write_history_marker("CanIncludeInClipboardHistory", 0);
    write_history_marker("CanUploadToCloudClipboard", 0);

    // NULL handle = delayed rendering: we are only asked for the data (via
    // WM_RENDERFORMAT) when a consumer actually reads it. SetClipboardData
    // returns the handle it was given, so for delayed rendering success is
    // also NULL and the windows crate reports it as an Err carrying
    // GetLastError(). Only a nonzero thread error is a real failure, and the
    // thread error must be cleared first so a stale value from an earlier
    // call can't masquerade as one.
    SetLastError(ERROR_SUCCESS);
    if let Err(e) = SetClipboardData(CF_UNICODETEXT.0 as u32, None) {
        if e.code().is_err() {
            return Err(format!("SetClipboardData failed: {e}"));
        }
    }
    Ok(())
}

fn on_timer(hwnd: HWND, shared: &WinTxShared) {
    let now = Instant::now();
    let finish = {
        let mut st = match shared.state.lock() {
            Ok(st) => st,
            Err(_) => return,
        };
        if st.cancelled {
            true
        } else {
            match evaluate(&st, now) {
                WaitDecision::KeepWaiting => false,
                WaitDecision::Finish => {
                    st.cancelled = true;
                    true
                }
            }
        }
    };
    if !finish {
        return;
    }

    let (receipt, ownership_lost, injection_failed) = {
        let st = match shared.state.lock() {
            Ok(st) => st,
            Err(_) => return,
        };
        (
            st.any_receipt_after_injection(),
            st.ownership_lost,
            st.injection_failed,
        )
    };
    if ownership_lost {
        info!("[reliable-paste] settling: clipboard ownership lost");
    } else if receipt {
        info!("[reliable-paste] settling: reads went quiet");
    } else if injection_failed {
        info!("[reliable-paste] settling: chord injection failed, restoring quickly");
    } else {
        info!("[reliable-paste] settling: no read within timeout, restoring anyway");
    }

    // Auto-submit only once the target demonstrably read the transcript;
    // pressing Enter after an unconfirmed paste could submit stale content.
    if shared.auto_submit && receipt {
        send_auto_submit(shared);
    }

    // Rendering the promised text can change the sequence number on a read.
    // Clipboard ownership is the stable guard against an external copy, even
    // if our own EmptyClipboard call sent WM_DESTROYCLIPBOARD during a retry.
    let still_ours = unsafe { GetClipboardOwner() }
        .map(|owner| owner == hwnd)
        .unwrap_or(false);
    if still_ours {
        if !unsafe { settle_clipboard(hwnd, shared) } {
            // Another process may have the clipboard open briefly. Keep the
            // owner window alive and retry on the next timer tick.
            return;
        }
    } else {
        info!("[reliable-paste] clipboard changed externally; leaving it untouched");
    }

    if let Ok(mut slot) = PENDING.lock() {
        let is_us = slot
            .as_ref()
            .map(|pending| Arc::as_ptr(pending) as *const WinTxShared == shared as *const _)
            .unwrap_or(false);
        if is_us {
            *slot = None;
        }
    }

    unsafe {
        PostQuitMessage(0);
    }
}

unsafe fn destroy_window_and_shared(hwnd: HWND) {
    let ptr = shared_ptr(hwnd);
    let _ = DestroyWindow(hwnd);
    if !ptr.is_null() {
        drop(Arc::from_raw(ptr));
    }
}

fn pump_thread(shared: Arc<WinTxShared>, ready: Sender<Result<(), String>>) {
    unsafe {
        let hinstance = match GetModuleHandleW(PCWSTR::null()) {
            Ok(hmodule) => HINSTANCE(hmodule.0),
            Err(e) => {
                let _ = ready.send(Err(format!("GetModuleHandle failed: {e}")));
                return;
            }
        };
        ensure_window_class(hinstance);

        let hwnd = match CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            CLASS_NAME,
            w!("HandyPasteTx"),
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(hinstance),
            None,
        ) {
            Ok(hwnd) => hwnd,
            Err(e) => {
                let _ = ready.send(Err(format!("CreateWindowEx failed: {e}")));
                return;
            }
        };
        SetWindowLongPtrW(
            hwnd,
            GWLP_USERDATA,
            Arc::into_raw(shared.clone()) as *const _ as isize,
        );

        let published = match snapshot_clipboard(hwnd, &shared) {
            Ok(()) => match publish(hwnd) {
                Ok(()) => Ok(()),
                Err(e) => {
                    // Publish may have emptied the clipboard before failing;
                    // restore the user's original contents if we still own it.
                    if GetClipboardOwner()
                        .map(|owner| owner == hwnd)
                        .unwrap_or(false)
                    {
                        let _ = restore_snapshot(hwnd, &shared);
                    }
                    Err(e)
                }
            },
            Err(e) => Err(e),
        };
        match published {
            Ok(()) => {}
            Err(e) => {
                destroy_window_and_shared(hwnd);
                let _ = ready.send(Err(e));
                return;
            }
        };
        shared.state.lock().unwrap().published_at = Instant::now();
        if let Ok(mut slot) = PENDING.lock() {
            *slot = Some(shared.clone());
        }
        if SetTimer(Some(hwnd), TIMER_ID, TIMER_INTERVAL_MS, None) == 0 {
            if GetClipboardOwner()
                .map(|owner| owner == hwnd)
                .unwrap_or(false)
            {
                let _ = restore_snapshot(hwnd, &shared);
            }
            if let Ok(mut slot) = PENDING.lock() {
                *slot = None;
            }
            destroy_window_and_shared(hwnd);
            let _ = ready.send(Err("Could not start reliable paste timer".to_string()));
            return;
        }
        let _ = ready.send(Ok(()));

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = DispatchMessageW(&msg);
        }

        let _ = KillTimer(Some(hwnd), TIMER_ID);
        destroy_window_and_shared(hwnd);
    }
}

pub(super) fn run(
    text: &str,
    app_handle: &tauri::AppHandle,
    paste_method: &PasteMethod,
    enigo: &mut enigo::Enigo,
    auto_submit: bool,
    auto_submit_key: AutoSubmitKey,
    clipboard_handling: ClipboardHandling,
) -> Result<(), String> {
    let shared = Arc::new(WinTxShared {
        state: Mutex::new(TxState::new()),
        text: text.to_string(),
        snapshot: Mutex::new(Vec::new()),
        saved_bitmap: Mutex::new(None),
        app_handle: app_handle.clone(),
        auto_submit,
        auto_submit_key,
        preserve_transcript: clipboard_handling == ClipboardHandling::CopyToClipboard,
    });

    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let shared_for_pump = shared.clone();
    thread::spawn(move || pump_thread(shared_for_pump, ready_tx));

    // Wait until the transcript is actually published (or the worker reports
    // why it could not) before injecting the chord.
    match ready_rx.recv() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(e),
        Err(_) => return Err("reliable paste worker died before publishing".to_string()),
    }
    info!("[reliable-paste] published transcript (delayed render)");

    // Mark injection *before* sending: enigo holds the chord for ~100ms and a
    // fast target may legitimately read while the chord is still held.
    shared.state.lock().unwrap().injected_at = Some(Instant::now());
    match send_chord(enigo, paste_method) {
        Ok(()) => {
            info!("[reliable-paste] paste chord sent ({paste_method:?})");
        }
        Err(e) => {
            // Keep the transaction alive: the worker restores the clipboard
            // after the short failed-injection timeout.
            shared.state.lock().unwrap().injection_failed = true;
            error!("[reliable-paste] failed to send paste chord: {e}");
            return Err(e);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::wait_for_previous;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn concurrent_paste_start_waits_for_first_caller() {
        let first = wait_for_previous().unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (acquired_tx, acquired_rx) = mpsc::channel();
        let second = thread::spawn(move || {
            started_tx.send(()).unwrap();
            let _second = wait_for_previous().unwrap();
            acquired_tx.send(()).unwrap();
        });

        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(acquired_rx.recv_timeout(Duration::from_millis(50)).is_err());
        drop(first);
        acquired_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        second.join().unwrap();
    }
}
