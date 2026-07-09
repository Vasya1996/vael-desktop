use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, RECT, TRUE};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClientRect, GetWindowThreadProcessId, IsWindowVisible,
};

#[derive(Clone, Copy)]
struct WinInfo {
    hwnd: isize,
    pid: u32,
    client_w: i32,
    client_h: i32,
}

/// A real game window is at least this big; filters tooltips / ghost windows.
pub fn is_candidate_window(client_w: i32, client_h: i32) -> bool {
    client_w >= 640 && client_h >= 480
}

extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: lparam carries a &mut Vec<WinInfo> set up in find_dota_hwnd.
    unsafe {
        let out = &mut *(lparam.0 as *mut Vec<WinInfo>);
        if !IsWindowVisible(hwnd).as_bool() {
            return TRUE;
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let mut rc = RECT::default();
        if GetClientRect(hwnd, &mut rc).is_ok() {
            out.push(WinInfo {
                hwnd: hwnd.0 as isize,
                pid,
                client_w: rc.right - rc.left,
                client_h: rc.bottom - rc.top,
            });
        }
        TRUE
    }
}

/// PIDs of every running dota2.exe (mirrors watch_dota in main.rs:553-561).
fn dota_pids() -> Vec<u32> {
    use sysinfo::System;
    let sys = System::new_all();
    sys.processes()
        .values()
        .filter(|p| p.name().to_string_lossy().eq_ignore_ascii_case("dota2.exe"))
        .map(|p| p.pid().as_u32())
        .collect()
}

/// The largest visible top-level window owned by a dota2.exe process, as an
/// HWND address (`isize`). None if Dota isn't running / has no visible window.
pub fn find_dota_hwnd() -> Option<isize> {
    let pids = dota_pids();
    if pids.is_empty() {
        return None;
    }
    let mut wins: Vec<WinInfo> = Vec::new();
    // SAFETY: enum_proc only touches the Vec during EnumWindows.
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut wins as *mut _ as isize));
    }
    wins.into_iter()
        .filter(|w| pids.contains(&w.pid) && is_candidate_window(w.client_w, w.client_h))
        .max_by_key(|w| (w.client_w as i64) * (w.client_h as i64))
        .map(|w| w.hwnd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predicate_filters_tiny_windows() {
        assert!(!is_candidate_window(0, 0));
        assert!(!is_candidate_window(300, 200));
        assert!(is_candidate_window(1280, 720));
        assert!(is_candidate_window(1920, 1080));
    }
}
