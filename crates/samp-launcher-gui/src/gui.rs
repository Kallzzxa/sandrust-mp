//! The Win32 window: a dark server browser in the shape of the original
//! samp.exe launcher (favorites list, nickname, connect) with a cleaner look
//! (dark theme, crimson accent, live server info). Only built on Windows;
//! the query and favorites logic it uses lives in the lib and is unit-tested
//! on any host.
//!
//! The GUI does not launch the game itself: it shells out to
//! `samp-launcher.exe` next to it (same folder) and streams that process's
//! output into the log box. It also points the child at
//! `data\auth_table.txt` next to the exe when the file exists, so the user
//! never has to set `SAMP_RS_AUTH_TABLE` by hand.

#![allow(non_snake_case)]
#![allow(unsafe_op_in_unsafe_fn)]

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use samp_launcher_gui::favorites::{self, Favorite};
use samp_launcher_gui::query::{self, ServerInfo};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, FillRect, SetBkMode, SetTextColor,
    DEFAULT_CHARSET, DEFAULT_PITCH, DEFAULT_QUALITY, DT_CENTER, DT_SINGLELINE, DT_VCENTER,
    FF_DONTCARE, FW_BOLD, FW_NORMAL, HBRUSH, HDC, HFONT, OUT_DEFAULT_PRECIS, TRANSPARENT,
};
use windows_sys::Win32::UI::Controls::{
    InitCommonControlsEx, DRAWITEMSTRUCT, EM_REPLACESEL, EM_SETSEL, ICC_LISTVIEW_CLASSES,
    INITCOMMONCONTROLSEX, LVCFMT_LEFT, LVCF_FMT, LVCF_TEXT, LVCF_WIDTH, LVIF_STATE, LVIF_TEXT,
    LVIS_SELECTED, LVM_DELETEALLITEMS, LVM_DELETEITEM, LVM_GETNEXTITEM, LVM_INSERTCOLUMNW,
    LVM_INSERTITEMW, LVM_SETBKCOLOR, LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMW, LVM_SETTEXTCOLOR,
    LVM_SETTEXTBKCOLOR, LVNI_SELECTED, LVN_ITEMCHANGED, LVCOLUMNW, LVITEMW, LVS_EX_FULLROWSELECT,
    LVS_EX_GRIDLINES, LVS_REPORT, LVS_SHOWSELALWAYS, LVS_SINGLESEL, NMLISTVIEW, ODS_SELECTED,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetDlgCtrlID, GetMessageW,
    GetWindowTextW, HMENU, LoadCursorW, LoadIconW, PostMessageW, PostQuitMessage,
    RegisterClassExW, SendMessageW, SetWindowLongPtrW, SetWindowTextW, ShowWindow,
    TranslateMessage, BN_CLICKED, BS_AUTOCHECKBOX, BS_OWNERDRAW, BS_PUSHBUTTON, CS_HREDRAW, CS_VREDRAW,
    CW_USEDEFAULT, ES_AUTOHSCROLL, ES_AUTOVSCROLL, ES_MULTILINE, ES_PASSWORD, ES_READONLY,
    GWLP_USERDATA, IDC_ARROW, IDI_APPLICATION, MSG, SW_SHOW, WM_CLOSE, WM_COMMAND, WM_CREATE,
    WM_CTLCOLORBTN, WM_CTLCOLOREDIT, WM_CTLCOLORSTATIC, WM_DESTROY, WM_DRAWITEM, WM_NCDESTROY,
    WM_NOTIFY, WM_SIZE, WM_USER, WNDCLASSEXW, WS_BORDER, WS_CHILD,
    WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

// Control ids.
const ID_LIST: i32 = 101;
const ID_NAME: i32 = 102;
const ID_HOST: i32 = 103;
const ID_PORT: i32 = 104;
const ID_PASS: i32 = 105;
const ID_DL: i32 = 106;
const ID_REFRESH: i32 = 107;
const ID_ADD: i32 = 108;
const ID_REMOVE: i32 = 109;
const ID_CONNECT: i32 = 110;
const ID_LOG: i32 = 111;
const ID_TITLE: i32 = 112;
const ID_SUBTITLE: i32 = 113;

// Custom messages: a query result is ready for row `wparam`; the launched
// child process exited.
const WM_QUERY_DONE: u32 = WM_USER + 1;
const WM_LAUNCH_DONE: u32 = WM_USER + 2;

// Palette.
const BG: u32 = 0x26201B; // dark navy (COLORREF is 0x00BBGGRR)
const PANEL: u32 = 0x2F2A23;
const EDIT_BG: u32 = 0x3A2A2A;
const TEXT: u32 = 0xF0E8E8;
const MUTED: u32 = 0xA89E9E;
const ACCENT: u32 = 0x2626DC; // crimson #DC2626
const ACCENT_DARK: u32 = 0x1A1AB8;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Query results shared between the UI thread and the refresh worker.
struct Shared {
    results: Vec<Option<ServerInfo>>,
}

struct State {
    hwnd: HWND,
    list: HWND,
    name_edit: HWND,
    host_edit: HWND,
    port_edit: HWND,
    pass_edit: HWND,
    dl_check: HWND,
    connect_btn: HWND,
    log_edit: HWND,
    title_font: HFONT,
    font: HFONT,
    log_font: HFONT,
    bg_brush: HBRUSH,
    edit_brush: HBRUSH,
    favorites: Vec<Favorite>,
    shared: Arc<Mutex<Shared>>,
    exe_dir: PathBuf,
    launching: bool,
}

fn exe_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())).unwrap_or_else(|| PathBuf::from("."))
}

pub fn run() {
    unsafe {
        let hinstance = windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(std::ptr::null());
        let class_name = wide("McRpLauncher");
        let bg_brush = CreateSolidBrush(BG);
        let cls = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance,
            hIcon: LoadIconW(std::ptr::null_mut(), IDI_APPLICATION),
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            hbrBackground: bg_brush,
            lpszMenuName: std::ptr::null(),
            lpszClassName: class_name.as_ptr(),
            hIconSm: LoadIconW(std::ptr::null_mut(), IDI_APPLICATION),
        };
        if RegisterClassExW(&cls) == 0 {
            return;
        }
        let _ = InitCommonControlsEx(&INITCOMMONCONTROLSEX { dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32, dwICC: ICC_LISTVIEW_CLASSES });
        let title = wide("MC-RP Launcher");
        let state = Box::new(State {
            hwnd: std::ptr::null_mut(),
            list: std::ptr::null_mut(),
            name_edit: std::ptr::null_mut(),
            host_edit: std::ptr::null_mut(),
            port_edit: std::ptr::null_mut(),
            pass_edit: std::ptr::null_mut(),
            dl_check: std::ptr::null_mut(),
            connect_btn: std::ptr::null_mut(),
            log_edit: std::ptr::null_mut(),
            title_font: std::ptr::null_mut(),
            font: std::ptr::null_mut(),
            log_font: std::ptr::null_mut(),
            bg_brush,
            edit_brush: CreateSolidBrush(EDIT_BG),
            favorites: Vec::new(),
            shared: Arc::new(Mutex::new(Shared { results: Vec::new() })),
            exe_dir: exe_dir(),
            launching: false,
        });
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            700,
            560,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinstance,
            Box::into_raw(state) as *const std::ffi::c_void,
        );
        if hwnd.is_null() {
            return;
        }
        ShowWindow(hwnd, SW_SHOW);
        let mut msg = MSG { hwnd: std::ptr::null_mut(), message: 0, wParam: 0, lParam: 0, time: 0, pt: windows_sys::Win32::Foundation::POINT { x: 0, y: 0 } };
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn get_state(hwnd: HWND) -> *mut State {
    unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            let cs = lparam as *const windows_sys::Win32::UI::WindowsAndMessaging::CREATESTRUCTW;
            let state = (*cs).lpCreateParams as *mut State;
            (*state).hwnd = hwnd;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as i32);
            on_create(&mut *state);
            0
        }
        WM_DESTROY => {
            let state = get_state(hwnd);
            if !state.is_null() {
                save_favorites(&*state);
            }
            PostQuitMessage(0);
            0
        }
        WM_NCDESTROY => {
            let state = get_state(hwnd);
            if !state.is_null() {
                let state = Box::from_raw(state);
                let _ = DeleteObject(state.title_font);
                let _ = DeleteObject(state.font);
                let _ = DeleteObject(state.log_font);
                let _ = DeleteObject(state.edit_brush);
                // bg_brush belongs to the window class; the process owns it.
            }
            0
        }
        WM_COMMAND => {
            let state = get_state(hwnd);
            if state.is_null() {
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }
            let id = (wparam & 0xFFFF) as i32;
            let code = ((wparam >> 16) & 0xFFFF) as u32;
            if code == BN_CLICKED {
                match id {
                    ID_REFRESH => refresh_all(&*state),
                    ID_ADD => on_add(&mut *state),
                    ID_REMOVE => on_remove(&mut *state),
                    ID_CONNECT => on_connect(&mut *state),
                    _ => {}
                }
            }
            0
        }
        WM_NOTIFY => {
            let state = get_state(hwnd);
            if state.is_null() {
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }
            let nm = lparam as *const NMLISTVIEW;
            if (*nm).hdr.code == LVN_ITEMCHANGED as u32
                && (*nm).uChanged & LVIF_STATE != 0
                && (*nm).uNewState & LVIS_SELECTED != 0
            {
                on_select(&*state, (*nm).iItem);
            }
            0
        }
        WM_CTLCOLORSTATIC => {
            let state = get_state(hwnd);
            let hdc = wparam as HDC;
            SetBkMode(hdc, TRANSPARENT as i32);
            let id = GetDlgCtrlID(lparam as HWND);
            let color = if id == ID_TITLE { ACCENT } else if id == ID_SUBTITLE { MUTED } else { TEXT };
            SetTextColor(hdc, color);
            if state.is_null() { 0 } else { (*state).bg_brush as isize }
        }
        WM_CTLCOLORBTN => {
            let state = get_state(hwnd);
            let hdc = wparam as HDC;
            SetBkMode(hdc, TRANSPARENT as i32);
            SetTextColor(hdc, TEXT);
            if state.is_null() { 0 } else { (*state).bg_brush as isize }
        }
        WM_CTLCOLOREDIT => {
            let state = get_state(hwnd);
            let hdc = wparam as HDC;
            windows_sys::Win32::Graphics::Gdi::SetBkColor(hdc, EDIT_BG);
            SetTextColor(hdc, TEXT);
            if state.is_null() { 0 } else { (*state).edit_brush as isize }
        }
        WM_DRAWITEM => {
            let state = get_state(hwnd);
            let dis = lparam as *const DRAWITEMSTRUCT;
            if !state.is_null() && (*dis).CtlID as i32 == ID_CONNECT {
                draw_connect_button(&*state, &*dis);
                return 1;
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_SIZE => {
            let state = get_state(hwnd);
            if !state.is_null() {
                on_resize(&*state);
            }
            0
        }
        WM_QUERY_DONE => {
            let state = get_state(hwnd);
            if !state.is_null() {
                update_row(&*state, wparam as usize);
            }
            0
        }
        WM_LAUNCH_DONE => {
            let state = get_state(hwnd);
            if !state.is_null() {
                (*state).launching = false;
                EnableWindow((*state).connect_btn, 1);
                append_log(&*state, "launcher exited.\r\n");
            }
            0
        }
        WM_CLOSE => {
            windows_sys::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn on_create(state: &mut State) {
    unsafe {
        let hinstance = windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(std::ptr::null());
        state.title_font = CreateFontW(-30, 0, 0, 0, FW_BOLD as i32, 0, 0, 0, DEFAULT_CHARSET as u32, OUT_DEFAULT_PRECIS as u32, 0, DEFAULT_QUALITY as u32, (DEFAULT_PITCH | FF_DONTCARE) as u32, wide("Segoe UI").as_ptr());
        state.font = CreateFontW(-15, 0, 0, 0, FW_NORMAL as i32, 0, 0, 0, DEFAULT_CHARSET as u32, OUT_DEFAULT_PRECIS as u32, 0, DEFAULT_QUALITY as u32, (DEFAULT_PITCH | FF_DONTCARE) as u32, wide("Segoe UI").as_ptr());
        state.log_font = CreateFontW(-13, 0, 0, 0, FW_NORMAL as i32, 0, 0, 0, DEFAULT_CHARSET as u32, OUT_DEFAULT_PRECIS as u32, 0, DEFAULT_QUALITY as u32, (DEFAULT_PITCH | FF_DONTCARE) as u32, wide("Consolas").as_ptr());
        let hwnd = state.hwnd;
        let font = state.font;
        let title_font = state.title_font;
        let log_font = state.log_font;
        let mk = |class: &str, text: &str, style: u32, x: i32, y: i32, w: i32, h: i32, id: i32| -> HWND {
            CreateWindowExW(0, wide(class).as_ptr(), wide(text).as_ptr(), style, x, y, w, h, hwnd, id as HMENU, hinstance, std::ptr::null())
        };
        let label = |text: &str, x: i32, y: i32, w: i32| -> HWND {
            let h = mk("STATIC", text, WS_CHILD | WS_VISIBLE, x, y, w, 18, 0);
            SendMessageW(h, windows_sys::Win32::UI::WindowsAndMessaging::WM_SETFONT, font as usize, 1);
            h
        };
        let title = mk("STATIC", "MC-RP", WS_CHILD | WS_VISIBLE, 16, 8, 200, 38, ID_TITLE);
        SendMessageW(title, windows_sys::Win32::UI::WindowsAndMessaging::WM_SETFONT, title_font as usize, 1);
        let sub = mk("STATIC", "Costum Launcher  •  SA-MP 0.3.DL-R1", WS_CHILD | WS_VISIBLE, 18, 46, 320, 18, ID_SUBTITLE);
        SendMessageW(sub, windows_sys::Win32::UI::WindowsAndMessaging::WM_SETFONT, font as usize, 1);
        state.dl_check = mk("BUTTON", "0.3.DL-R1 (custom models)", WS_CHILD | WS_VISIBLE | BS_AUTOCHECKBOX as u32, 470, 18, 200, 20, ID_DL);
        SendMessageW(state.dl_check, windows_sys::Win32::UI::WindowsAndMessaging::WM_SETFONT, font as usize, 1);
        SendMessageW(state.dl_check, windows_sys::Win32::UI::WindowsAndMessaging::BM_SETCHECK, 1, 0);
        label("Nickname", 16, 78, 70);
        state.name_edit = mk("EDIT", "", WS_CHILD | WS_VISIBLE | WS_BORDER | WS_TABSTOP | ES_AUTOHSCROLL as u32, 92, 74, 170, 22, ID_NAME);
        SendMessageW(state.name_edit, windows_sys::Win32::UI::WindowsAndMessaging::WM_SETFONT, font as usize, 1);
        label("Host", 282, 78, 40);
        state.host_edit = mk("EDIT", "", WS_CHILD | WS_VISIBLE | WS_BORDER | WS_TABSTOP | ES_AUTOHSCROLL as u32, 322, 74, 170, 22, ID_HOST);
        SendMessageW(state.host_edit, windows_sys::Win32::UI::WindowsAndMessaging::WM_SETFONT, font as usize, 1);
        label("Port", 512, 78, 30);
        state.port_edit = mk("EDIT", "7777", WS_CHILD | WS_VISIBLE | WS_BORDER | WS_TABSTOP | ES_AUTOHSCROLL as u32, 546, 74, 60, 22, ID_PORT);
        SendMessageW(state.port_edit, windows_sys::Win32::UI::WindowsAndMessaging::WM_SETFONT, font as usize, 1);
        label("Password", 16, 108, 70);
        state.pass_edit = mk("EDIT", "", WS_CHILD | WS_VISIBLE | WS_BORDER | WS_TABSTOP | ES_AUTOHSCROLL as u32 | ES_PASSWORD as u32, 92, 104, 170, 22, ID_PASS);
        SendMessageW(state.pass_edit, windows_sys::Win32::UI::WindowsAndMessaging::WM_SETFONT, font as usize, 1);
        let mk_btn = |text: &str, x: i32, y: i32, w: i32, id: i32| -> HWND {
            let h = mk("BUTTON", text, WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON as u32, x, y, w, 26, id);
            SendMessageW(h, windows_sys::Win32::UI::WindowsAndMessaging::WM_SETFONT, font as usize, 1);
            h
        };
        mk_btn("Refresh", 16, 336, 90, ID_REFRESH);
        mk_btn("Add", 114, 336, 90, ID_ADD);
        mk_btn("Remove", 212, 336, 90, ID_REMOVE);
        state.connect_btn = mk("BUTTON", "Connect", WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_OWNERDRAW as u32, 520, 332, 164, 32, ID_CONNECT);
        SendMessageW(state.connect_btn, windows_sys::Win32::UI::WindowsAndMessaging::WM_SETFONT, font as usize, 1);
        state.list = mk("SysListView32", "", WS_CHILD | WS_VISIBLE | WS_BORDER | WS_TABSTOP | LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS, 16, 136, 668, 190, ID_LIST);
        SendMessageW(state.list, LVM_SETEXTENDEDLISTVIEWSTYLE, 0, (LVS_EX_FULLROWSELECT | LVS_EX_GRIDLINES) as isize);
        SendMessageW(state.list, LVM_SETBKCOLOR, 0, PANEL as isize);
        SendMessageW(state.list, LVM_SETTEXTBKCOLOR, 0, PANEL as isize);
        SendMessageW(state.list, LVM_SETTEXTCOLOR, 0, TEXT as isize);
        let cols = [("Server", 250), ("Host", 210), ("Players", 90), ("Ping", 80)];
        for (i, (name, width)) in cols.iter().enumerate() {
            let text = wide(name);
            let col = LVCOLUMNW { mask: LVCF_FMT | LVCF_WIDTH | LVCF_TEXT, fmt: LVCFMT_LEFT, cx: *width, pszText: text.as_ptr() as *mut u16, cchTextMax: 0, iSubItem: 0, iImage: 0, iOrder: 0, cxMin: 0, cxDefault: 0, cxIdeal: 0 };
            SendMessageW(state.list, LVM_INSERTCOLUMNW, i, &col as *const LVCOLUMNW as isize);
        }
        state.log_edit = mk("EDIT", "", WS_CHILD | WS_VISIBLE | WS_BORDER | WS_VSCROLL | ES_MULTILINE as u32 | ES_READONLY as u32 | ES_AUTOVSCROLL as u32, 16, 392, 668, 138, ID_LOG);
        SendMessageW(state.log_edit, windows_sys::Win32::UI::WindowsAndMessaging::WM_SETFONT, log_font as usize, 1);
        // Favorites: load, or start with the default server.
        let fav_path = state.exe_dir.join("favorites.txt");
        state.favorites = std::fs::read_to_string(&fav_path).map(|t| favorites::parse(&t)).unwrap_or_else(|_| favorites::defaults());
        if !fav_path.exists() {
            let _ = std::fs::write(&fav_path, favorites::serialize(&state.favorites));
        }
        state.shared.lock().unwrap().results = vec![None; state.favorites.len()];
        populate_list(state);
        if let Some(first) = state.favorites.first() {
            set_edit_text(state.host_edit, &first.host);
            set_edit_text(state.port_edit, &first.port.to_string());
        }
        append_log(state, "MC-RP Launcher ready.\r\n");
        refresh_all(state);
    }
}

fn save_favorites(state: &State) {
    let _ = std::fs::write(state.exe_dir.join("favorites.txt"), favorites::serialize(&state.favorites));
}

fn set_edit_text(hwnd: HWND, text: &str) {
    unsafe { SetWindowTextW(hwnd, wide(text).as_ptr()) };
}

fn edit_text(hwnd: HWND) -> String {
    unsafe {
        let mut buf = vec![0u16; 512];
        let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        if n <= 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..n as usize])
    }
}

fn populate_list(state: &State) {
    unsafe {
        SendMessageW(state.list, LVM_DELETEALLITEMS, 0, 0);
        for (i, fav) in state.favorites.iter().enumerate() {
            insert_row(state, i, fav);
        }
    }
}

fn insert_row(state: &State, i: usize, fav: &Favorite) {
    unsafe {
        let name = wide(&fav.name);
        let item = LVITEMW { mask: LVIF_TEXT, iItem: i as i32, iSubItem: 0, state: 0, stateMask: 0, pszText: name.as_ptr() as *mut u16, cchTextMax: 0, iImage: 0, lParam: 0, iIndent: 0, iGroupId: 0, cColumns: 0, puColumns: std::ptr::null_mut(), piColFmt: std::ptr::null_mut(), iGroup: 0 };
        SendMessageW(state.list, LVM_INSERTITEMW, 0, &item as *const LVITEMW as isize);
        set_cell(state, i, 1, &format!("{}:{}", fav.host, fav.port));
        set_cell(state, i, 2, "—");
        set_cell(state, i, 3, "—");
    }
}

fn set_cell(state: &State, row: usize, col: i32, text: &str) {
    unsafe {
        let text = wide(text);
        let item = LVITEMW { mask: LVIF_TEXT, iItem: row as i32, iSubItem: col, state: 0, stateMask: 0, pszText: text.as_ptr() as *mut u16, cchTextMax: 0, iImage: 0, lParam: 0, iIndent: 0, iGroupId: 0, cColumns: 0, puColumns: std::ptr::null_mut(), piColFmt: std::ptr::null_mut(), iGroup: 0 };
        SendMessageW(state.list, LVM_SETITEMW, 0, &item as *const LVITEMW as isize);
    }
}

fn update_row(state: &State, idx: usize) {
    let info = state.shared.lock().unwrap().results.get(idx).cloned().flatten();
    match info {
        Some(info) => {
            let name = if info.hostname.is_empty() { state.favorites[idx].name.clone() } else { info.hostname.clone() };
            set_cell(state, idx, 0, &name);
            set_cell(state, idx, 2, &format!("{}/{}", info.players, info.max_players));
            set_cell(state, idx, 3, &format!("{} ms", info.ping_ms));
        }
        None => {
            set_cell(state, idx, 2, "offline");
            set_cell(state, idx, 3, "—");
        }
    }
}

/// Query every favorite on a worker thread; results come back as WM_QUERY_DONE.
fn refresh_all(state: &State) {
    // HWND is a raw pointer and not Send: pass it across threads as a number.
    let hwnd = state.hwnd as usize;
    let favs = state.favorites.clone();
    let shared = Arc::clone(&state.shared);
    std::thread::spawn(move || {
        for (i, fav) in favs.iter().enumerate() {
            let info = query::query_server(&fav.host, fav.port).ok().flatten();
            if let Ok(mut g) = shared.lock() {
                if i < g.results.len() {
                    g.results[i] = info;
                }
            }
            unsafe { PostMessageW(hwnd as HWND, WM_QUERY_DONE, i, 0) };
        }
    });
}

fn selected_row(state: &State) -> Option<usize> {
    unsafe {
        let idx = SendMessageW(state.list, LVM_GETNEXTITEM, usize::MAX, LVNI_SELECTED as isize);
        if idx < 0 { None } else { Some(idx as usize) }
    }
}

fn on_select(state: &State, row: i32) {
    if let Some(fav) = state.favorites.get(row as usize) {
        set_edit_text(state.host_edit, &fav.host);
        set_edit_text(state.port_edit, &fav.port.to_string());
    }
}

fn on_add(state: &mut State) {
    let host = edit_text(state.host_edit);
    let port: u16 = edit_text(state.port_edit).parse().unwrap_or(0);
    if host.is_empty() || port == 0 {
        append_log(state, "Add: fill in Host and Port first.\r\n");
        return;
    }
    if state.favorites.iter().any(|f| f.host == host && f.port == port) {
        append_log(state, "Add: that server is already in the list.\r\n");
        return;
    }
    let name = state
        .shared
        .lock()
        .unwrap()
        .results
        .iter()
        .zip(state.favorites.iter())
        .find(|(_, f)| f.host == host && f.port == port)
        .and_then(|(info, _)| info.as_ref().map(|i| i.hostname.clone()))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| host.clone());
    state.favorites.push(Favorite::new(&name, &host, port));
    state.shared.lock().unwrap().results.push(None);
    let idx = state.favorites.len() - 1;
    insert_row(state, idx, &state.favorites[idx]);
    save_favorites(state);
    append_log(state, &format!("Added {} ({}:{}).\r\n", name, host, port));
    refresh_all(state);
}

fn on_remove(state: &mut State) {
    let Some(idx) = selected_row(state) else {
        append_log(state, "Remove: select a server in the list first.\r\n");
        return;
    };
    let removed = state.favorites.remove(idx);
    state.shared.lock().unwrap().results.remove(idx);
    unsafe { SendMessageW(state.list, LVM_DELETEITEM, idx, 0) };
    save_favorites(state);
    append_log(state, &format!("Removed {}.\r\n", removed.name));
}

fn on_connect(state: &mut State) {
    if state.launching {
        return;
    }
    let name = edit_text(state.name_edit);
    let host = edit_text(state.host_edit);
    let port_s = edit_text(state.port_edit);
    let pass = edit_text(state.pass_edit);
    if name.is_empty() || name.contains(char::is_whitespace) {
        append_log(state, "Connect: nickname must be non-empty without spaces.\r\n");
        return;
    }
    let Ok(port) = port_s.parse::<u16>() else {
        append_log(state, "Connect: port must be 1..65535.\r\n");
        return;
    };
    if host.is_empty() {
        append_log(state, "Connect: host is empty.\r\n");
        return;
    }
    let dl = unsafe { SendMessageW(state.dl_check, windows_sys::Win32::UI::WindowsAndMessaging::BM_GETCHECK, 0, 0) == 1 };
    let launcher = state.exe_dir.join("samp-launcher.exe");
    if !launcher.exists() {
        append_log(state, "Connect: samp-launcher.exe not found next to the launcher.\r\n");
        return;
    }
    let mut args: Vec<String> = vec![
        "--gta".into(),
        state.exe_dir.join("gta_sa.exe").to_string_lossy().into_owned(),
        "--dll".into(),
        state.exe_dir.join("samp_client.dll").to_string_lossy().into_owned(),
        "--name".into(),
        name.clone(),
        "--host".into(),
        host.clone(),
        "--port".into(),
        port.to_string(),
    ];
    if !pass.is_empty() {
        args.push("--password".into());
        args.push(pass);
    }
    if dl {
        args.push("--dl".into());
    }
    append_log(state, &format!("Connecting as {} to {}:{} ({}).\r\n", name, host, port, if dl { "0.3.DL-R1" } else { "0.3.7-R4" }));
    state.launching = true;
    unsafe { EnableWindow(state.connect_btn, 0) };
    let hwnd = state.hwnd as usize;
    let log_hwnd = state.log_edit as usize;
    let mut cmd = Command::new(&launcher);
    cmd.args(&args).current_dir(&state.exe_dir).stdout(Stdio::piped()).stderr(Stdio::piped());
    let auth_table = state.exe_dir.join("data").join("auth_table.txt");
    if auth_table.exists() {
        cmd.env("SAMP_RS_AUTH_TABLE", &auth_table);
    }
    match cmd.spawn() {
        Ok(mut child) => {
            if let Some(out) = child.stdout.take() {
                let log_hwnd = log_hwnd;
                std::thread::spawn(move || pump_log(Box::new(out), log_hwnd));
            }
            if let Some(err) = child.stderr.take() {
                let log_hwnd = log_hwnd;
                std::thread::spawn(move || pump_log(Box::new(err), log_hwnd));
            }
            std::thread::spawn(move || {
                let _ = child.wait();
                unsafe { PostMessageW(hwnd as HWND, WM_LAUNCH_DONE, 0, 0) };
            });
        }
        Err(e) => {
            state.launching = false;
            unsafe { EnableWindow(state.connect_btn, 1) };
            append_log(state, &format!("Connect: could not start samp-launcher.exe: {}.\r\n", e));
        }
    }
}

fn append_log(state: &State, text: &str) {
    append_log_hwnd(state.log_edit, text);
}

/// Stream one child-process pipe into the log box, line by line.
/// `log_hwnd` is passed as a number because HWND (a raw pointer) is not Send.
fn pump_log(stream: Box<dyn std::io::Read + Send>, log_hwnd: usize) {
    let log_hwnd = log_hwnd as HWND;
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        match line {
            Ok(line) => append_log_hwnd(log_hwnd, &format!("{}\r\n", line)),
            Err(_) => break,
        }
    }
}

fn append_log_hwnd(hwnd: HWND, text: &str) {
    unsafe {
        // Caret to the end (-1, -1), then insert: appends and auto-scrolls.
        SendMessageW(hwnd, EM_SETSEL, usize::MAX, -1);
        let text = wide(text);
        SendMessageW(hwnd, EM_REPLACESEL, 0, text.as_ptr() as isize);
    }
}

fn draw_connect_button(state: &State, dis: &DRAWITEMSTRUCT) {
    unsafe {
        let pressed = dis.itemState & ODS_SELECTED != 0;
        let brush = CreateSolidBrush(if pressed { ACCENT_DARK } else { ACCENT });
        FillRect(dis.hDC, &dis.rcItem, brush);
        DeleteObject(brush);
        SetBkMode(dis.hDC, TRANSPARENT as i32);
        SetTextColor(dis.hDC, 0xFFFFFF);
        let mut rc = dis.rcItem;
        let text = wide("Connect");
        // Use the window font for the button label.
        let old = windows_sys::Win32::Graphics::Gdi::SelectObject(dis.hDC, state.font);
        DrawTextW(dis.hDC, text.as_ptr(), -1, &mut rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        windows_sys::Win32::Graphics::Gdi::SelectObject(dis.hDC, old);
    }
}

fn on_resize(state: &State) {
    unsafe {
        let mut rc = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(state.hwnd, &mut rc);
        let w = rc.right - rc.left;
        let h = rc.bottom - rc.top;
        if w <= 0 || h <= 0 {
            return;
        }
        let move_win = |hwnd: HWND, x: i32, y: i32, ww: i32, hh: i32| {
            windows_sys::Win32::UI::WindowsAndMessaging::MoveWindow(hwnd, x, y, ww, hh, 1);
        };
        move_win(state.list, 16, 136, w - 32, 190);
        move_win(state.connect_btn, w - 180, 332, 164, 32);
        move_win(state.log_edit, 16, 392, w - 32, h - 392 - 14);
    }
}
