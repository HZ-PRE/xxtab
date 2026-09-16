//! Native Windows GUI. All HWND/GDI operations stay on the message-loop thread.
#![allow(unsafe_op_in_unsafe_fn)]
use anyhow::{Context, Result};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    mem::size_of,
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    sync::{Arc, Mutex, OnceLock},
    thread::JoinHandle,
    time::Instant,
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::{
        LibraryLoader::GetModuleHandleW,
        SystemServices::{SS_ENDELLIPSIS, SS_PATHELLIPSIS},
    },
    UI::{
        Controls::{Dialogs::*, *},
        HiDpi::*,
        Input::KeyboardAndMouse::{EnableWindow, SetFocus},
        Shell::*,
        WindowsAndMessaging::*,
    },
};
use xxtab::{
    logging::{self, Feed, Status},
    profiles::{Draft, Store},
};

const CONFIG: usize = 100;
const CONNECT: usize = 101;
const DISCONNECT: usize = 102;
const RECONNECT: usize = 103;
const NEW: usize = 104;
const IMPORT: usize = 105;
const EDIT: usize = 106;
const CLEAR: usize = 107;
const EXIT: usize = 108;
const ABOUT: usize = 109;
const VIEW: usize = 110;
const UPDATE: usize = 111;
const SAVE: usize = 205;
const CANCEL: usize = 206;
const IMPORT_WG: usize = 207;
const TRAY_EVENT: u32 = WM_APP + 1;
const RESTORE_WINDOW: u32 = WM_APP + 2;
const TRAY_ID: u32 = 1;
const TRAY_KEY_SELECT: u32 = NIN_SELECT | NINF_KEY;
static TASKBAR_CREATED: OnceLock<u32> = OnceLock::new();

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
    static EDITOR: RefCell<Option<Editor>> = const { RefCell::new(None) };
    static TRAY_RECREATE_PENDING: Cell<bool> = const { Cell::new(false) };
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
unsafe fn set_text(hwnd: HWND, text: &str) {
    SetWindowTextW(hwnd, wide(text).as_ptr());
}
unsafe fn text(hwnd: HWND) -> String {
    let length = GetWindowTextLengthW(hwnd).max(0) as usize;
    let mut buf = vec![0u16; length + 1];
    let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}
fn s(hwnd: HWND, n: i32) -> i32 {
    unsafe { n * GetDpiForWindow(hwnd).max(96) as i32 / 96 }
}
unsafe fn control(parent: HWND, class: &str, label: &str, id: usize, style: u32, ex: u32) -> HWND {
    CreateWindowExW(
        ex,
        wide(class).as_ptr(),
        wide(label).as_ptr(),
        WS_CHILD | WS_VISIBLE | style,
        0,
        0,
        0,
        0,
        parent,
        id as HMENU,
        GetModuleHandleW(null()),
        null(),
    )
}
unsafe fn place(hwnd: HWND, parent: HWND, x: i32, y: i32, w: i32, h: i32) {
    MoveWindow(
        hwnd,
        s(parent, x),
        s(parent, y),
        s(parent, w.max(1)),
        s(parent, h.max(1)),
        1,
    );
}
unsafe fn dimensions(hwnd: HWND) -> (i32, i32) {
    let mut r = RECT::default();
    GetClientRect(hwnd, &mut r);
    let dpi = GetDpiForWindow(hwnd).max(96) as i32;
    (r.right * 96 / dpi, r.bottom * 96 / dpi)
}
unsafe fn make_font(hwnd: HWND, mono: bool) -> HFONT {
    CreateFontW(
        -s(hwnd, if mono { 13 } else { 14 }),
        0,
        0,
        0,
        400,
        0,
        0,
        0,
        DEFAULT_CHARSET as u32,
        OUT_DEFAULT_PRECIS as u32,
        CLIP_DEFAULT_PRECIS as u32,
        CLEARTYPE_QUALITY as u32,
        DEFAULT_PITCH as u32,
        wide(if mono {
            "Consolas"
        } else {
            "Microsoft YaHei UI"
        })
        .as_ptr(),
    )
}
unsafe fn error(hwnd: HWND, error: impl std::fmt::Display) {
    MessageBoxW(
        hwnd,
        wide(&error.to_string()).as_ptr(),
        wide("xxtab — 操作未完成").as_ptr(),
        MB_OK | MB_ICONERROR,
    );
}
unsafe fn open_file(hwnd: HWND, wg_only: bool) -> Option<PathBuf> {
    let mut filename = [0u16; 32768];
    let filter = wide(if wg_only {
        "WireGuard 配置 (*.conf)\0*.conf\0所有文件\0*.*\0"
    } else {
        "隧道 / WireGuard 配置 (*.toml;*.conf)\0*.toml;*.conf\0所有文件\0*.*\0"
    });
    let mut dialog: OPENFILENAMEW = std::mem::zeroed();
    dialog.lStructSize = size_of::<OPENFILENAMEW>() as u32;
    dialog.hwndOwner = hwnd;
    dialog.lpstrFilter = filter.as_ptr();
    dialog.lpstrFile = filename.as_mut_ptr();
    dialog.nMaxFile = filename.len() as u32;
    dialog.Flags = OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR | OFN_EXPLORER;
    if GetOpenFileNameW(&mut dialog) != 0 {
        Some(PathBuf::from(String::from_utf16_lossy(
            &filename[..filename
                .iter()
                .position(|x| *x == 0)
                .unwrap_or(filename.len())],
        )))
    } else {
        None
    }
}

struct Tray {
    hwnd: HWND,
    added: bool,
    tip: &'static str,
    menu_open: bool,
}
impl Tray {
    unsafe fn new(hwnd: HWND) -> Self {
        let taskbar_created = *TASKBAR_CREATED
            .get_or_init(|| RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()));
        if taskbar_created != 0 {
            // Explorer runs at normal integrity even when this GUI is elevated.
            ChangeWindowMessageFilterEx(hwnd, taskbar_created, MSGFLT_ALLOW, null_mut());
        }
        ChangeWindowMessageFilterEx(hwnd, TRAY_EVENT, MSGFLT_ALLOW, null_mut());
        Self {
            hwnd,
            added: false,
            tip: "xxtab · 未连接",
            menu_open: false,
        }
    }
    unsafe fn data(&self) -> NOTIFYICONDATAW {
        let mut data = tray_identity(self.hwnd);
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        data.uCallbackMessage = TRAY_EVENT;
        data.hIcon = GetClassLongPtrW(self.hwnd, GCLP_HICON) as HICON;
        for (to, from) in data.szTip.iter_mut().take(127).zip(self.tip.encode_utf16()) {
            *to = from;
        }
        data
    }
    unsafe fn add(&mut self) -> bool {
        if !self.added {
            let mut data = self.data();
            self.added = Shell_NotifyIconW(NIM_ADD, &data) != 0;
            if self.added {
                data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
                Shell_NotifyIconW(NIM_SETVERSION, &data);
            }
        }
        self.added
    }
    unsafe fn update(&mut self, tip: &'static str) {
        if self.tip != tip {
            self.tip = tip;
            if self.added {
                Shell_NotifyIconW(NIM_MODIFY, &self.data());
            }
        }
    }
}
impl Drop for Tray {
    fn drop(&mut self) {
        if self.added {
            unsafe { Shell_NotifyIconW(NIM_DELETE, &tray_identity(self.hwnd)) };
        }
    }
}
fn tray_identity(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ID,
        ..Default::default()
    }
}
struct PopupMenu(HMENU);
impl Drop for PopupMenu {
    fn drop(&mut self) {
        unsafe { DestroyMenu(self.0) };
    }
}
unsafe fn restore_window(hwnd: HWND) {
    // ShowWindow sends WM_SIZE synchronously: do not hold a UI RefCell borrow.
    ShowWindow(
        hwnd,
        if IsIconic(hwnd) != 0 {
            SW_RESTORE
        } else {
            SW_SHOW
        },
    );
    let editor = EDITOR.with(|slot| {
        slot.try_borrow()
            .ok()
            .and_then(|slot| slot.as_ref().map(|e| e.hwnd))
    });
    let active = editor.unwrap_or(hwnd);
    ShowWindow(active, SW_SHOW);
    SetForegroundWindow(active);
}
unsafe fn show_tray_menu(hwnd: HWND) {
    let menu = UI.with(|slot| {
        let mut slot = slot.try_borrow_mut().ok()?;
        let ui = slot.as_mut()?;
        if ui.tray.menu_open {
            return None;
        }
        let menu = ui.tray_menu()?;
        ui.tray.menu_open = true;
        Some(menu)
    });
    let Some(menu) = menu else { return };
    let mut point = POINT::default();
    GetCursorPos(&mut point);
    SetForegroundWindow(hwnd);
    // TrackPopupMenu runs a nested message loop. Let timers/cleanup keep running.
    let command = TrackPopupMenu(
        menu.0,
        TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
        point.x,
        point.y,
        0,
        hwnd,
        null(),
    );
    UI.with(|slot| {
        if let Some(ui) = slot.borrow_mut().as_mut() {
            ui.tray.menu_open = false;
        }
    });
    if IsWindow(hwnd) != 0 {
        PostMessageW(hwnd, WM_NULL, 0, 0);
        if command != 0 {
            PostMessageW(hwnd, WM_COMMAND, command as usize, 0);
        }
    }
}

struct Worker {
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
    thread: JoinHandle<Result<()>>,
}
enum UpdateResult {
    Checked(xxtab::update::Check),
    Downloaded(xxtab::update::Download),
}
struct Ui {
    hwnd: HWND,
    tray: Tray,
    combo: HWND,
    status: HWND,
    detail: HWND,
    path: HWND,
    log: HWND,
    connect: HWND,
    disconnect: HWND,
    reconnect: HWND,
    new: HWND,
    import: HWND,
    edit: HWND,
    view: HWND,
    clear: HWND,
    log_label: HWND,
    font: HFONT,
    mono: HFONT,
    store: Store,
    feed: Arc<Mutex<Feed>>,
    worker: Option<Worker>,
    updater: Option<JoinHandle<Result<UpdateResult>>>,
    pending_install: Option<xxtab::update::Download>,
    history: VecDeque<String>,
    status_value: Status,
    connected_at: Option<Instant>,
    stopping: bool,
    restart: bool,
    closing: bool,
}
impl Ui {
    unsafe fn create(hwnd: HWND, store: Store, feed: Arc<Mutex<Feed>>) -> Self {
        let font = make_font(hwnd, false);
        let mono = make_font(hwnd, true);
        let combo = control(
            hwnd,
            "COMBOBOX",
            "",
            CONFIG,
            WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST as u32,
            0,
        );
        let status = control(hwnd, "STATIC", "● 未连接", 0, 0, 0);
        let detail = control(
            hwnd,
            "STATIC",
            "导入已有配置，或新建一份配置后连接。",
            0,
            SS_ENDELLIPSIS,
            0,
        );
        let path = control(hwnd, "STATIC", "", 0, SS_PATHELLIPSIS, 0);
        let log = control(
            hwnd,
            "EDIT",
            "",
            0,
            WS_TABSTOP
                | WS_VSCROLL
                | ES_MULTILINE as u32
                | ES_AUTOVSCROLL as u32
                | ES_READONLY as u32,
            WS_EX_CLIENTEDGE,
        );
        SendMessageW(log, EM_SETLIMITTEXT, 65536, 0);
        let button = |label, id| {
            control(
                hwnd,
                "BUTTON",
                label,
                id,
                WS_TABSTOP | BS_PUSHBUTTON as u32,
                0,
            )
        };
        let mut ui = Self {
            hwnd,
            tray: Tray::new(hwnd),
            combo,
            status,
            detail,
            path,
            log,
            connect: button("连接", CONNECT),
            disconnect: button("断开", DISCONNECT),
            reconnect: button("重新连接", RECONNECT),
            new: button("新建配置", NEW),
            import: button("导入配置…", IMPORT),
            edit: button("编辑配置…", EDIT),
            view: button("查看当前配置", VIEW),
            clear: button("清空日志", CLEAR),
            log_label: control(hwnd, "STATIC", "运行日志", 0, 0, 0),
            font,
            mono,
            store,
            feed,
            worker: None,
            updater: None,
            pending_install: None,
            history: VecDeque::new(),
            status_value: Status::Idle,
            connected_at: None,
            stopping: false,
            restart: false,
            closing: false,
        };
        for c in [
            ui.combo,
            ui.status,
            ui.detail,
            ui.path,
            ui.connect,
            ui.disconnect,
            ui.reconnect,
            ui.new,
            ui.import,
            ui.edit,
            ui.view,
            ui.clear,
            ui.log_label,
        ] {
            SendMessageW(c, WM_SETFONT, font as usize, 1);
        }
        SendMessageW(log, WM_SETFONT, mono as usize, 1);
        ui.refresh_profiles();
        ui.layout();
        ui.buttons();
        ui.append("xxtab 已就绪。配置保存在当前用户的本地应用数据目录。");
        ui.tray.add();
        ui
    }
    unsafe fn layout(&self) {
        let (w, h) = dimensions(self.hwnd);
        place(self.combo, self.hwnd, 12, 12, w - 148, 250);
        place(self.status, self.hwnd, 14, 49, w - 150, 23);
        place(self.detail, self.hwnd, 14, 78, w - 150, 23);
        for (c, y) in [
            (self.connect, 12),
            (self.disconnect, 45),
            (self.reconnect, 78),
        ] {
            place(c, self.hwnd, w - 124, y, 112, 27);
        }
        place(self.new, self.hwnd, 12, 114, 102, 28);
        place(self.import, self.hwnd, 122, 114, 106, 28);
        place(self.edit, self.hwnd, 236, 114, 106, 28);
        place(self.view, self.hwnd, 350, 114, 122, 28);
        place(self.log_label, self.hwnd, 14, 157, 160, 24);
        place(self.clear, self.hwnd, w - 112, 151, 100, 27);
        place(self.log, self.hwnd, 12, 184, w - 24, h - 219);
        place(self.path, self.hwnd, 14, h - 27, w - 28, 20);
    }
    fn selected(&self) -> Option<usize> {
        let index = unsafe { SendMessageW(self.combo, CB_GETCURSEL, 0, 0) };
        (index >= 0 && (index as usize) < self.store.catalog.profiles.len())
            .then_some(index as usize)
    }
    unsafe fn refresh_profiles(&mut self) {
        SendMessageW(self.combo, CB_RESETCONTENT, 0, 0);
        for p in &self.store.catalog.profiles {
            SendMessageW(self.combo, CB_ADDSTRING, 0, wide(&p.name).as_ptr() as isize);
        }
        if !self.store.catalog.profiles.is_empty() {
            self.store.catalog.selected = self
                .store
                .catalog
                .selected
                .min(self.store.catalog.profiles.len() - 1);
            SendMessageW(self.combo, CB_SETCURSEL, self.store.catalog.selected, 0);
        }
        self.summary();
        self.buttons();
    }
    unsafe fn summary(&self) {
        if let Some(index) = self.selected()
            && let Ok(path) = self.store.path(index)
        {
            set_text(self.path, &path.to_string_lossy());
            if let Ok(cfg) = xxtab::config::Config::load(&path) {
                let address = self
                    .store
                    .load(index)
                    .ok()
                    .and_then(|d| {
                        d.wireguard.lines().find_map(|l| {
                            let (k, v) = l.split_once('=')?;
                            (k.trim() == "Address").then(|| v.trim().to_owned())
                        })
                    })
                    .unwrap_or_default();
                set_text(self.detail, &format!("{address}    ·    {}", cfg.server));
            }
        }
    }
    unsafe fn buttons(&self) {
        let busy = self.worker.is_some();
        let selected = self.selected().is_some();
        EnableWindow(self.combo, (!busy) as i32);
        for c in [self.new, self.import] {
            EnableWindow(c, (!busy) as i32);
        }
        EnableWindow(self.edit, (!busy && selected) as i32);
        EnableWindow(self.view, selected as i32);
        EnableWindow(self.connect, self.connection_action_enabled(CONNECT) as i32);
        EnableWindow(
            self.disconnect,
            self.connection_action_enabled(DISCONNECT) as i32,
        );
        EnableWindow(
            self.reconnect,
            self.connection_action_enabled(RECONNECT) as i32,
        );
        let menu = GetMenu(self.hwnd);
        EnableMenuItem(
            menu,
            UPDATE as u32,
            MF_BYCOMMAND
                | if self.updater.is_some() || self.closing {
                    MF_GRAYED
                } else {
                    MF_ENABLED
                },
        );
        EnableMenuItem(
            menu,
            VIEW as u32,
            MF_BYCOMMAND | if selected { MF_ENABLED } else { MF_GRAYED },
        );
        for id in [NEW, IMPORT, EDIT] {
            EnableMenuItem(
                menu,
                id as u32,
                MF_BYCOMMAND
                    | if busy || (id == EDIT && !selected) {
                        MF_GRAYED
                    } else {
                        MF_ENABLED
                    },
            );
        }
    }
    fn connection_action_enabled(&self, id: usize) -> bool {
        if self.closing || self.stopping {
            return false;
        }
        match id {
            CONNECT => self.worker.is_none() && self.selected().is_some(),
            DISCONNECT | RECONNECT => self.worker.is_some(),
            _ => false,
        }
    }
    unsafe fn tray_menu(&self) -> Option<PopupMenu> {
        let handle = CreatePopupMenu();
        if handle.is_null() {
            return None;
        }
        let menu = PopupMenu(handle);
        let available =
            windows_sys::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(self.hwnd) != 0;
        for (id, label) in [
            (CONNECT, "连接"),
            (DISCONNECT, "断开"),
            (RECONNECT, "重新连接"),
            (EXIT, "退出"),
        ] {
            let enabled = available
                && if id == EXIT {
                    !self.closing
                } else {
                    self.connection_action_enabled(id)
                };
            AppendMenuW(
                menu.0,
                MF_STRING | if enabled { MF_ENABLED } else { MF_GRAYED },
                id,
                wide(label).as_ptr(),
            );
        }
        Some(menu)
    }
    unsafe fn append(&mut self, line: &str) {
        // Bounded visible history; no file logging of configuration contents/private keys.
        let mut local: windows_sys::Win32::Foundation::SYSTEMTIME = std::mem::zeroed();
        windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut local);
        let clean: String = line
            .chars()
            .filter(|c| *c != '\r' && *c != '\n')
            .take(1024)
            .collect();
        self.history.push_back(format!(
            "[{:02}:{:02}:{:02}] {clean}",
            local.wHour, local.wMinute, local.wSecond
        ));
        while self.history.len() > 256
            || self.history.iter().map(String::len).sum::<usize>() > 48 * 1024
        {
            self.history.pop_front();
        }
    }
    unsafe fn paint_log(&self) {
        let value = self
            .history
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("\r\n");
        set_text(self.log, &value);
        SendMessageW(self.log, EM_SETSEL, usize::MAX, -1);
        SendMessageW(self.log, EM_SCROLLCARET, 0, 0);
    }
    unsafe fn start(&mut self) -> Result<()> {
        if self.worker.is_some() {
            return Ok(());
        }
        let index = self.selected().context("请先导入或新建配置")?;
        let path = self.store.path(index)?;
        self.store.catalog.selected = index;
        self.store.persist()?;
        let (send, receive) = tokio::sync::oneshot::channel();
        self.feed.lock().unwrap().status = Status::Connecting;
        self.connected_at = None;
        self.stopping = false;
        self.status_value = Status::Connecting;
        self.append("开始连接：正在检查配置并建立隧道…");
        let thread = std::thread::Builder::new()
            .name("xxtab-tunnel".into())
            .stack_size(1024 * 1024)
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                runtime.block_on(xxtab::app::run(&path, "run", async {
                    let _ = receive.await;
                    Ok(())
                }))
            })?;
        self.worker = Some(Worker {
            cancel: Some(send),
            thread,
        });
        self.buttons();
        self.tick();
        Ok(())
    }
    unsafe fn stop(&mut self, restart: bool) {
        self.restart = restart;
        if let Some(worker) = &mut self.worker {
            if let Some(cancel) = worker.cancel.take() {
                let _ = cancel.send(());
            }
            self.stopping = true;
            self.append(if restart {
                "正在断开，清理完成后重新连接…"
            } else {
                "正在断开并清理 WireGuard 接口与路由…"
            });
            self.buttons();
            self.paint_log();
        }
    }
    unsafe fn tick(&mut self) {
        self.update_tick();
        // A broadcast can arrive inside a modal command's nested message loop.
        if TRAY_RECREATE_PENDING.with(|pending| pending.replace(false)) {
            self.tray.added = false;
            if !self.tray.add() {
                PostMessageW(self.hwnd, RESTORE_WINDOW, 0, 0);
            }
        }
        let (lines, status) = {
            let mut feed = self.feed.lock().unwrap_or_else(|e| e.into_inner());
            (feed.lines.drain(..).collect::<Vec<_>>(), feed.status)
        };
        let dirty = !lines.is_empty();
        for line in lines {
            self.append(&line);
        }
        if self.status_value != status {
            self.status_value = status;
            if status == Status::Connected {
                self.connected_at = Some(Instant::now());
            }
        }
        if self.worker.as_ref().is_some_and(|w| w.thread.is_finished()) {
            let worker = self.worker.take().unwrap();
            let result = worker
                .thread
                .join()
                .unwrap_or_else(|_| Err(anyhow::anyhow!("连接线程异常结束")));
            self.stopping = false;
            self.connected_at = None;
            let success = result.is_ok();
            match result {
                Ok(()) => {
                    self.status_value = Status::Idle;
                    self.append("已断开连接，清理完成。");
                }
                Err(e) => {
                    self.status_value = Status::Failed;
                    self.append(&format!("连接失败：{e:#}"));
                }
            }
            self.feed.lock().unwrap().status = self.status_value;
            self.buttons();
            self.paint_log();
            if self.closing {
                if self.pending_install.is_some() {
                    if !success {
                        self.closing = false;
                        self.pending_install = None;
                        self.append("连接清理失败，已取消安装更新。请查看日志。");
                        self.buttons();
                        self.paint_log();
                        return;
                    }
                    if let Err(error) = self.launch_update() {
                        self.closing = false;
                        self.append(&format!("无法安装更新：{error:#}"));
                        self.buttons();
                        self.paint_log();
                        return;
                    }
                }
                DestroyWindow(self.hwnd);
                return;
            }
            if std::mem::take(&mut self.restart) && success {
                if let Err(e) = self.start() {
                    error(self.hwnd, e);
                }
                return;
            }
        }
        let state = if self.stopping {
            "● 正在断开…".into()
        } else {
            match self.status_value {
                Status::Idle => "● 未连接".into(),
                Status::Connecting => "● 正在连接…".into(),
                Status::Reconnecting => "● 连接中断，正在重连…".into(),
                Status::Failed => "● 连接失败，请查看日志".into(),
                Status::Connected => {
                    let t = self
                        .connected_at
                        .map(|t| t.elapsed().as_secs())
                        .unwrap_or(0);
                    format!(
                        "● 隧道已连接    {:02}:{:02}:{:02}",
                        t / 3600,
                        (t / 60) % 60,
                        t % 60
                    )
                }
            }
        };
        set_text(self.status, &state);
        self.tray.update(if self.closing {
            "xxtab · 正在退出"
        } else if self.stopping {
            "xxtab · 正在断开"
        } else {
            match self.status_value {
                Status::Idle => "xxtab · 未连接",
                Status::Connecting => "xxtab · 正在连接",
                Status::Connected => "xxtab · 隧道已连接",
                Status::Reconnecting => "xxtab · 正在重连",
                Status::Failed => "xxtab · 连接失败，点击查看日志",
            }
        });
        if dirty {
            self.paint_log();
        }
    }
    unsafe fn start_update(&mut self, release: Option<xxtab::update::Release>) -> Result<()> {
        if self.updater.is_some() || self.closing {
            return Ok(());
        }
        self.append(if release.is_some() {
            "正在从 GitHub 下载更新并校验 SHA256…"
        } else {
            "正在检查 GitHub Releases…"
        });
        self.paint_log();
        self.updater = Some(
            std::thread::Builder::new()
                .name("xxtab-update".into())
                .stack_size(1024 * 1024)
                .spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    runtime.block_on(async {
                        match release {
                            Some(release) => Ok(UpdateResult::Downloaded(
                                xxtab::update::download(&release).await?,
                            )),
                            None => Ok(UpdateResult::Checked(xxtab::update::check().await?)),
                        }
                    })
                })?,
        );
        self.buttons();
        Ok(())
    }
    unsafe fn launch_update(&mut self) -> Result<()> {
        if let Some(download) = self.pending_install.take() {
            xxtab::update::install_after_exit(&download)?;
        }
        Ok(())
    }
    unsafe fn update_tick(&mut self) {
        if self.closing
            || windows_sys::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(self.hwnd) == 0
            || !self
                .updater
                .as_ref()
                .is_some_and(|thread| thread.is_finished())
        {
            return;
        }
        let result = self
            .updater
            .take()
            .unwrap()
            .join()
            .unwrap_or_else(|_| Err(anyhow::anyhow!("更新线程异常结束")));
        self.buttons();
        let result = (|| -> Result<()> {
            match result? {
                UpdateResult::Checked(check) => {
                    if check.available {
                        let release = check.release.context("缺少更新信息")?;
                        let message = format!(
                            "当前版本：{}\n新版本：{}\n\n从 GitHub Releases 下载更新？\n下载完成后将再次询问是否安装。",
                            check.current, release.version
                        );
                        if MessageBoxW(
                            self.hwnd,
                            wide(&message).as_ptr(),
                            wide("发现新版本").as_ptr(),
                            MB_YESNO | MB_ICONINFORMATION,
                        ) == IDYES
                        {
                            self.start_update(Some(release))?;
                        }
                    } else {
                        let message = if check.release.is_none() {
                            format!("当前版本：{}\nGitHub 尚未发布正式版本。", check.current)
                        } else {
                            format!("当前版本：{}\n没有更新的正式版本。", check.current)
                        };
                        MessageBoxW(
                            self.hwnd,
                            wide(&message).as_ptr(),
                            wide("检查更新").as_ptr(),
                            MB_OK | MB_ICONINFORMATION,
                        );
                    }
                }
                UpdateResult::Downloaded(download) => {
                    self.append(&format!(
                        "更新 {} 下载完成，SHA256 校验通过：{}",
                        download.version,
                        download.path.display()
                    ));
                    self.paint_log();
                    if MessageBoxW(self.hwnd, wide("更新包校验通过。\n立即断开连接并退出，打开更新安装向导？\n已有配置会保留。").as_ptr(), wide("安装更新").as_ptr(), MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2) == IDYES {
                        self.pending_install = Some(download);
                        if self.worker.is_some() { self.closing = true; self.stop(false); }
                        else { self.launch_update()?; PostMessageW(self.hwnd, WM_CLOSE, 0, 0); }
                    }
                }
            }
            Ok(())
        })();
        if let Err(problem) = result {
            self.append(&format!("更新失败：{problem:#}"));
            self.paint_log();
            error(self.hwnd, format!("更新失败：{problem:#}"));
        }
    }
    unsafe fn command(&mut self, id: usize) -> Result<()> {
        if matches!(id, CONNECT | DISCONNECT | RECONNECT) && !self.connection_action_enabled(id) {
            return Ok(());
        }
        if self.worker.is_some() && matches!(id, NEW | IMPORT | EDIT | CONFIG) {
            return Ok(());
        }
        match id {
            UPDATE => self.start_update(None)?,
            CONNECT => self.start()?,
            DISCONNECT => self.stop(false),
            RECONNECT => self.stop(true),
            NEW => show_editor(
                self.hwnd,
                None,
                Draft {
                    name: "新配置".into(),
                    tunnel: xxtab::profiles::TEMPLATE.into(),
                    wireguard: xxtab::profiles::WG_TEMPLATE.into(),
                },
                false,
            )?,
            IMPORT => {
                if let Some(path) = open_file(self.hwnd, false) {
                    show_editor(self.hwnd, None, xxtab::profiles::import(&path)?, false)?;
                }
            }
            EDIT => {
                if let Some(index) = self.selected() {
                    show_editor(self.hwnd, Some(index), self.store.load(index)?, false)?;
                }
            }
            VIEW => {
                if let Some(index) = self.selected() {
                    show_editor(self.hwnd, Some(index), self.store.load(index)?, true)?;
                }
            }
            CONFIG => {
                if let Some(index) = self.selected() {
                    self.store.catalog.selected = index;
                    self.store.persist()?;
                }
                self.summary();
                self.buttons();
            }
            CLEAR => {
                self.history.clear();
                self.paint_log();
            }
            EXIT => {
                PostMessageW(self.hwnd, WM_CLOSE, 0, 0);
            }
            ABOUT => {
                MessageBoxW(self.hwnd,wide("xxtab 0.1.0\n\n系统 WireGuard + 内置 wstunnel 传输\nWindows 原生界面 · 无浏览器运行时\n\n“隧道已连接”表示外层连接和本地接口已就绪。\n实际 VPN 可达性仍取决于 WireGuard 服务端配置。").as_ptr(),wide("关于 xxtab").as_ptr(),MB_OK|MB_ICONINFORMATION);
            }
            _ => {}
        }
        Ok(())
    }
}
impl Drop for Ui {
    fn drop(&mut self) {
        unsafe {
            DeleteObject(self.font);
            DeleteObject(self.mono);
        }
    }
}

struct Editor {
    read_only: bool,
    hwnd: HWND,
    owner: HWND,
    index: Option<usize>,
    name: HWND,
    tunnel: HWND,
    wg: HWND,
    name_label: HWND,
    tunnel_label: HWND,
    wg_label: HWND,
    hint: HWND,
    save: HWND,
    cancel: HWND,
    import: HWND,
    font: HFONT,
    mono: HFONT,
    original: (String, String, String),
}
impl Editor {
    unsafe fn layout(&self) {
        let (w, h) = dimensions(self.hwnd);
        let half = (h - 170) / 2;
        place(self.name_label, self.hwnd, 12, 15, 72, 24);
        place(self.name, self.hwnd, 88, 12, w - 100, 27);
        place(self.tunnel_label, self.hwnd, 12, 51, w - 24, 22);
        place(self.tunnel, self.hwnd, 12, 76, w - 24, half);
        place(self.wg_label, self.hwnd, 12, 87 + half, w - 164, 25);
        place(self.import, self.hwnd, w - 150, 83 + half, 138, 27);
        place(self.wg, self.hwnd, 12, 115 + half, w - 24, half);
        place(self.hint, self.hwnd, 12, h - 42, w - 238, 30);
        place(self.save, self.hwnd, w - 220, h - 42, 112, 29);
        place(self.cancel, self.hwnd, w - 100, h - 42, 88, 29);
    }
    unsafe fn draft(&self) -> Draft {
        Draft {
            name: text(self.name),
            tunnel: text(self.tunnel).replace("\r\n", "\n"),
            wireguard: text(self.wg).replace("\r\n", "\n"),
        }
    }
    unsafe fn changed(&self) -> bool {
        let d = self.draft();
        (d.name, d.tunnel, d.wireguard) != self.original
    }
    unsafe fn command(&mut self, id: usize) -> Result<bool> {
        if self.read_only {
            return Ok(id == CANCEL);
        }
        match id {
            SAVE => {
                let draft = self.draft();
                UI.with(|slot| -> Result<()> {
                    let mut ui = slot.borrow_mut();
                    let ui = ui.as_mut().context("主窗口不可用")?;
                    ui.store.save(self.index, &draft)?;
                    ui.refresh_profiles();
                    ui.append("配置已保存到本地副本，原始导入文件保持不变。");
                    ui.paint_log();
                    Ok(())
                })?;
                Ok(true)
            }
            CANCEL => Ok(self.confirm_close()),
            IMPORT_WG => {
                if let Some(path) = open_file(self.hwnd, true) {
                    let content = xxtab::profiles::read_text(&path)?;
                    set_text(self.wg, &content.replace('\n', "\r\n"));
                }
                Ok(false)
            }
            _ => Ok(false),
        }
    }
    unsafe fn confirm_close(&self) -> bool {
        self.read_only
            || !self.changed()
            || MessageBoxW(
                self.hwnd,
                wide("配置尚未保存，放弃这次修改？").as_ptr(),
                wide("关闭配置编辑器").as_ptr(),
                MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2,
            ) == IDYES
    }
}
impl Drop for Editor {
    fn drop(&mut self) {
        unsafe {
            DeleteObject(self.font);
            DeleteObject(self.mono);
        }
    }
}
unsafe fn show_editor(
    owner: HWND,
    index: Option<usize>,
    draft: Draft,
    read_only: bool,
) -> Result<()> {
    let class = wide("XxtabEditor");
    let hwnd = CreateWindowExW(
        WS_EX_CONTROLPARENT,
        class.as_ptr(),
        wide(if read_only {
            "当前配置（只读）— xxtab"
        } else if index.is_some() {
            "编辑配置 — xxtab"
        } else {
            "新建 / 导入配置 — xxtab"
        })
        .as_ptr(),
        WS_OVERLAPPEDWINDOW & !WS_MINIMIZEBOX,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        s(owner, 760),
        s(owner, 690),
        owner,
        null_mut(),
        GetModuleHandleW(null()),
        null(),
    );
    anyhow::ensure!(!hwnd.is_null(), "无法创建配置窗口");
    let font = make_font(hwnd, false);
    let mono = make_font(hwnd, true);
    let label = |caption| control(hwnd, "STATIC", caption, 0, 0, 0);
    let name = control(
        hwnd,
        "EDIT",
        &draft.name,
        201,
        WS_TABSTOP | ES_AUTOHSCROLL as u32,
        WS_EX_CLIENTEDGE,
    );
    SendMessageW(name, EM_SETLIMITTEXT, 60, 0);
    let edit_style = WS_TABSTOP
        | WS_VSCROLL
        | WS_HSCROLL
        | ES_MULTILINE as u32
        | ES_AUTOVSCROLL as u32
        | ES_AUTOHSCROLL as u32
        | ES_WANTRETURN as u32;
    let tunnel = control(
        hwnd,
        "EDIT",
        &draft.tunnel.replace('\n', "\r\n"),
        202,
        edit_style,
        WS_EX_CLIENTEDGE,
    );
    let wg = control(
        hwnd,
        "EDIT",
        &draft.wireguard.replace('\n', "\r\n"),
        203,
        edit_style,
        WS_EX_CLIENTEDGE,
    );
    for edit in [tunnel, wg] {
        SendMessageW(edit, EM_SETLIMITTEXT, 128 * 1024, 0);
        SendMessageW(edit, WM_SETFONT, mono as usize, 1);
    }
    let button = |caption, id| {
        control(
            hwnd,
            "BUTTON",
            caption,
            id,
            WS_TABSTOP | BS_PUSHBUTTON as u32,
            0,
        )
    };
    let editor = Editor {
        read_only,
        hwnd,
        owner,
        index,
        name,
        tunnel,
        wg,
        name_label: label("配置名称"),
        tunnel_label: label("隧道配置 · xxtab.toml"),
        wg_label: label("WireGuard 配置 · wg.conf"),
        hint: label(if read_only {
            "当前选中的已保存配置，只读。"
        } else {
            "保存时会检查配置格式和密钥。"
        }),
        save: button("保存并关闭", SAVE),
        cancel: button(if read_only { "关闭" } else { "取消" }, CANCEL),
        import: button("导入 WG 配置…", IMPORT_WG),
        font,
        mono,
        original: (draft.name, draft.tunnel, draft.wireguard),
    };
    for c in [
        editor.name,
        editor.name_label,
        editor.tunnel_label,
        editor.wg_label,
        editor.hint,
        editor.save,
        editor.cancel,
        editor.import,
    ] {
        SendMessageW(c, WM_SETFONT, font as usize, 1);
    }
    editor.layout();
    if read_only {
        for field in [editor.name, editor.tunnel, editor.wg] {
            SendMessageW(field, EM_SETREADONLY, 1, 0);
        }
        ShowWindow(editor.save, SW_HIDE);
        ShowWindow(editor.import, SW_HIDE);
    }
    EDITOR.with(|slot| *slot.borrow_mut() = Some(editor));
    EnableWindow(owner, 0);
    ShowWindow(hwnd, SW_SHOW);
    SetFocus(name);
    Ok(())
}

unsafe extern "system" fn editor_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let mut close = false;
    match msg {
        WM_COMMAND | WM_CLOSE | WM_SIZE => {
            EDITOR.with(|slot| {
                if let Ok(mut slot) = slot.try_borrow_mut()
                    && let Some(editor) = slot.as_mut()
                {
                    if msg == WM_SIZE {
                        editor.layout();
                    } else if msg == WM_CLOSE {
                        close = editor.confirm_close();
                    } else {
                        match editor.command(w & 0xffff) {
                            Ok(value) => close = value,
                            Err(e) => error(hwnd, format!("{e:#}")),
                        }
                    }
                }
            });
            if close {
                let owner = EDITOR.with(|slot| {
                    slot.borrow()
                        .as_ref()
                        .map(|e| e.owner)
                        .unwrap_or(null_mut())
                });
                EnableWindow(owner, 1);
                DestroyWindow(hwnd);
                EDITOR.with(|slot| slot.borrow_mut().take());
                SetForegroundWindow(owner);
            }
            0
        }
        WM_GETMINMAXINFO => {
            let info = &mut *(l as *mut MINMAXINFO);
            info.ptMinTrackSize = POINT {
                x: s(hwnd, 620),
                y: s(hwnd, 570),
            };
            0
        }
        WM_DPICHANGED => {
            let r = &*(l as *const RECT);
            SetWindowPos(
                hwnd,
                null_mut(),
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                SWP_NOZORDER,
            );
            0
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}
unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg != 0 && TASKBAR_CREATED.get() == Some(&msg) {
        let restored = UI.with(|slot| {
            let Ok(mut slot) = slot.try_borrow_mut() else {
                TRAY_RECREATE_PENDING.with(|pending| pending.set(true));
                return true;
            };
            let Some(ui) = slot.as_mut() else {
                return true;
            };
            ui.tray.added = false;
            ui.tray.add()
        });
        if !restored {
            restore_window(hwnd);
        }
        return 0;
    }
    match msg {
        TRAY_EVENT => {
            // A native modal dialog can be running inside a Ui/Editor command.
            if UI.with(|slot| slot.try_borrow_mut().is_err())
                || EDITOR.with(|slot| slot.try_borrow_mut().is_err())
            {
                return 0;
            }
            match (l as u32) & 0xffff {
                WM_CONTEXTMENU | WM_RBUTTONUP => show_tray_menu(hwnd),
                NIN_SELECT | TRAY_KEY_SELECT | WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
                    restore_window(hwnd)
                }
                _ => {}
            }
            0
        }
        RESTORE_WINDOW => {
            restore_window(hwnd);
            0
        }
        WM_COMMAND | WM_TIMER | WM_SIZE | WM_CLOSE => {
            // Native dialogs run nested message loops. Reject reentrant mutable access.
            UI.with(|slot| {
                if let Ok(mut slot) = slot.try_borrow_mut()
                    && let Some(ui) = slot.as_mut()
                {
                    match msg {
                        WM_COMMAND => {
                            let id = w & 0xffff;
                            if windows_sys::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(
                                hwnd,
                            ) == 0
                            {
                                return;
                            }
                            if id == EXIT && !ui.closing {
                                PostMessageW(hwnd, WM_CLOSE, 0, 0);
                            } else if (id != CONFIG
                                || ((w >> 16) & 0xffff) == CBN_SELCHANGE as usize)
                                && let Err(e) = ui.command(id)
                            {
                                ui.append(&format!("操作失败：{e:#}"));
                                ui.paint_log();
                                error(hwnd, format!("{e:#}"));
                            }
                        }
                        WM_TIMER => ui.tick(),
                        WM_SIZE => {
                            if w == SIZE_MINIMIZED as usize {
                                if ui.tray.add() {
                                    ShowWindow(hwnd, SW_HIDE);
                                } else {
                                    ui.append("无法添加系统托盘图标，保留主窗口。");
                                    ui.paint_log();
                                    PostMessageW(hwnd, RESTORE_WINDOW, 0, 0);
                                }
                            } else {
                                ui.layout();
                            }
                        }
                        WM_CLOSE => {
                            if ui.worker.is_some() {
                                ui.closing = true;
                                ui.stop(false);
                            } else {
                                DestroyWindow(hwnd);
                            }
                        }
                        _ => {}
                    }
                }
            });
            0
        }
        WM_CTLCOLORSTATIC => {
            let color = UI.with(|slot| {
                slot.try_borrow()
                    .ok()
                    .and_then(|slot| {
                        slot.as_ref().map(|ui| {
                            if l as HWND == ui.status {
                                Some(match ui.status_value {
                                    Status::Connected => 0x388E00,
                                    Status::Failed => 0x3030C0,
                                    Status::Connecting | Status::Reconnecting => 0x0080A0,
                                    _ => 0x666666,
                                })
                            } else {
                                None
                            }
                        })
                    })
                    .flatten()
            });
            if let Some(color) = color {
                SetTextColor(w as HDC, color);
                SetBkColor(w as HDC, GetSysColor(COLOR_BTNFACE));
                return GetSysColorBrush(COLOR_BTNFACE) as isize;
            }
            DefWindowProcW(hwnd, msg, w, l)
        }
        WM_GETMINMAXINFO => {
            let info = &mut *(l as *mut MINMAXINFO);
            info.ptMinTrackSize = POINT {
                x: s(hwnd, 610),
                y: s(hwnd, 360),
            };
            0
        }
        WM_DPICHANGED => {
            let r = &*(l as *const RECT);
            SetWindowPos(
                hwnd,
                null_mut(),
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                SWP_NOZORDER,
            );
            0
        }
        WM_DESTROY => {
            // DestroyWindow can run while Ui is mutably borrowed by tick().
            Shell_NotifyIconW(NIM_DELETE, &tray_identity(hwnd));
            KillTimer(hwnd, 1);
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}

pub fn run() {
    unsafe {
        if let Err(e) = run_inner() {
            error(null_mut(), format!("{e:#}"));
        }
    }
}
unsafe fn create_main(store: Store, feed: Arc<Mutex<Feed>>) -> Result<HWND> {
    let instance = GetModuleHandleW(null());
    // Win32 MAKEINTRESOURCEW(1): this value is a resource ID, never a dereferenced pointer.
    #[allow(clippy::manual_dangling_ptr)]
    let icon = LoadIconW(instance, 1usize as *const u16);
    for (name, proc) in [
        (
            "XxtabMain",
            window_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
        ),
        ("XxtabEditor", editor_proc),
    ] {
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(proc),
            hInstance: instance,
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hIcon: icon,
            hbrBackground: GetSysColorBrush(COLOR_BTNFACE),
            lpszClassName: null(),
            ..std::mem::zeroed()
        };
        // Keep class name allocation alive through registration.
        let class_name = wide(name);
        let class = WNDCLASSW {
            lpszClassName: class_name.as_ptr(),
            ..class
        };
        anyhow::ensure!(RegisterClassW(&class) != 0, "无法注册窗口类");
    }
    let menu = CreateMenu();
    let file = CreatePopupMenu();
    for (id, label) in [
        (NEW, "新建配置"),
        (IMPORT, "导入配置…"),
        (EDIT, "编辑配置…"),
        (VIEW, "查看当前配置"),
        (EXIT, "退出"),
    ] {
        AppendMenuW(file, MF_STRING, id, wide(label).as_ptr());
    }
    AppendMenuW(menu, MF_POPUP, file as usize, wide("文件").as_ptr());
    AppendMenuW(menu, MF_STRING, UPDATE, wide("检查更新").as_ptr());
    AppendMenuW(menu, MF_STRING, ABOUT, wide("关于").as_ptr());
    let hwnd = CreateWindowExW(
        WS_EX_CONTROLPARENT,
        wide("XxtabMain").as_ptr(),
        wide("xxtab — WireGuard 隧道客户端").as_ptr(),
        WS_OVERLAPPEDWINDOW,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        760,
        530,
        null_mut(),
        menu,
        instance,
        null(),
    );
    anyhow::ensure!(!hwnd.is_null(), "无法创建主窗口");
    UI.with(|slot| *slot.borrow_mut() = Some(Ui::create(hwnd, store, feed)));
    UI.with(|slot| slot.borrow().as_ref().unwrap().paint_log());
    Ok(hwnd)
}
unsafe fn run_inner() -> Result<()> {
    SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    let root = std::env::var_os("LOCALAPPDATA").context("无法获取当前用户的 LocalAppData")?;
    let store = Store::open(Path::new(&root).join("xxtab").join("profiles"))?;
    let feed = Arc::new(Mutex::new(Feed::default()));
    logging::attach(feed.clone());
    let hwnd = create_main(store, feed)?;
    SetTimer(hwnd, 1, 250, None);
    ShowWindow(hwnd, SW_SHOW);
    let mut msg: MSG = std::mem::zeroed();
    while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
        let editor = EDITOR.with(|slot| slot.borrow().as_ref().map(|e| e.hwnd));
        let target = editor.unwrap_or(hwnd);
        if IsDialogMessageW(target, &msg) == 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    UI.with(|slot| slot.borrow_mut().take());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        net::TcpListener,
        process::{Child, Command, Stdio},
        time::Duration,
    };
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled;

    thread_local! {
        static POPUP_PUMPED: Cell<bool> = const { Cell::new(false) };
    }
    unsafe extern "system" fn dismiss_test_menu(hwnd: HWND, _: u32, id: usize, _: u32) {
        KillTimer(hwnd, id);
        // The real popup's nested loop must not keep Ui borrowed and block timers.
        POPUP_PUMPED.with(|result| result.set(UI.with(|slot| slot.try_borrow_mut().is_ok())));
        PostMessageW(hwnd, WM_CANCELMODE, 0, 0);
    }

    struct Cleanup(Child, HWND);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            UI.with(|slot| {
                if let Some(mut ui) = slot.borrow_mut().take()
                    && let Some(mut worker) = ui.worker.take()
                {
                    if let Some(cancel) = worker.cancel.take() {
                        let _ = cancel.send(());
                    }
                    let _ = worker.thread.join();
                }
            });
            unsafe {
                if IsWindow(self.1) != 0 {
                    DestroyWindow(self.1);
                }
            }
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    unsafe fn pump_until(mut ready: impl FnMut() -> bool) {
        let end = Instant::now() + Duration::from_secs(30);
        loop {
            let mut message: MSG = std::mem::zeroed();
            while PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            UI.with(|slot| {
                if let Some(ui) = slot.borrow_mut().as_mut() {
                    ui.tick();
                }
            });
            if ready() {
                return;
            }
            assert!(
                Instant::now() < end,
                "GUI timeout: {}",
                UI.with(|slot| slot
                    .borrow()
                    .as_ref()
                    .map(|ui| ui.history.iter().cloned().collect::<Vec<_>>().join("\n"))
                    .unwrap_or_default())
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    unsafe fn assert_tray_menu(enabled: [bool; 4]) {
        UI.with(|slot| {
            let slot = slot.borrow();
            let menu = slot.as_ref().unwrap().tray_menu().unwrap();
            assert_eq!(GetMenuItemCount(menu.0), 4);
            for ((id, label), enabled) in [
                (CONNECT, "连接"),
                (DISCONNECT, "断开"),
                (RECONNECT, "重新连接"),
                (EXIT, "退出"),
            ]
            .into_iter()
            .zip(enabled)
            {
                assert_eq!(
                    GetMenuState(menu.0, id as u32, MF_BYCOMMAND) & MF_GRAYED == 0,
                    enabled
                );
                let mut name = [0u16; 32];
                let length = GetMenuStringW(
                    menu.0,
                    id as u32,
                    name.as_mut_ptr(),
                    name.len() as i32,
                    MF_BYCOMMAND,
                );
                assert_eq!(String::from_utf16_lossy(&name[..length as usize]), label);
            }
        });
    }
    unsafe fn tray_exists(hwnd: HWND) -> bool {
        let identity = NOTIFYICONIDENTIFIER {
            cbSize: size_of::<NOTIFYICONIDENTIFIER>() as u32,
            hWnd: hwnd,
            uID: TRAY_ID,
            ..Default::default()
        };
        Shell_NotifyIconGetRect(&identity, &mut RECT::default()) >= 0
    }
    // Capture this test's own native window into a bitmap; no desktop or other apps are read.
    unsafe fn capture(hwnd: HWND, path: &Path) {
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect);
        let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
        let dc = CreateCompatibleDC(null_mut());
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = width;
        info.bmiHeader.biHeight = -height;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB;
        let mut bits = null_mut();
        let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        assert!(!bitmap.is_null());
        let old = SelectObject(dc, bitmap);
        assert_ne!(
            windows_sys::Win32::Storage::Xps::PrintWindow(hwnd, dc, 0),
            0
        );
        let count = (width * height * 4) as usize;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"BM");
        bytes.extend_from_slice(&((54 + count) as u32).to_le_bytes());
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(&54u32.to_le_bytes());
        bytes.extend_from_slice(&40u32.to_le_bytes());
        bytes.extend_from_slice(&width.to_le_bytes());
        bytes.extend_from_slice(&(-height).to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&32u16.to_le_bytes());
        bytes.extend_from_slice(&[0; 24]);
        bytes.extend_from_slice(std::slice::from_raw_parts(bits as *const u8, count));
        std::fs::write(path, bytes).unwrap();
        SelectObject(dc, old);
        DeleteObject(bitmap);
        DeleteDC(dc);
    }

    #[test]
    #[ignore = "native Windows GUI / real WireGuard lifecycle; requires admin and WSTUNNEL_BIN"]
    fn native_controls_connection_editor_and_shutdown() {
        unsafe {
            use base64::Engine;
            SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let temp = tempfile::tempdir().unwrap();
            let mut store = Store::open(temp.path().join("profiles")).unwrap();
            let port = TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port();
            let udp = std::net::UdpSocket::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port();
            let key = base64::engine::general_purpose::STANDARD.encode([1; 32]);
            let draft = Draft {
                name: "本地测试连接".into(),
                tunnel: format!(
                    "server='ws://127.0.0.1:{port}'\nlisten='127.0.0.1:{udp}'\n[wireguard]\nconfig='wg.conf'\nname='xxtaguitest'"
                ),
                wireguard: format!(
                    "[Interface]\nPrivateKey={key}\nAddress=10.254.251.1/32\n[Peer]\nPublicKey={key}\nAllowedIPs=10.254.251.2/32\n"
                ),
            };
            store.save(None, &draft).unwrap();
            let feed = Arc::new(Mutex::new(Feed::default()));
            logging::attach(feed.clone());
            let hwnd = create_main(store, feed).unwrap();
            let mut command =
                Command::new(std::env::var_os("WSTUNNEL_BIN").expect("set WSTUNNEL_BIN"));
            use std::os::windows::process::CommandExt;
            command
                .creation_flags(0x08000000)
                .env_remove("NO_COLOR")
                .args([
                    "server",
                    "--restrict-to",
                    "127.0.0.1:7007",
                    &format!("ws://127.0.0.1:{port}"),
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let _cleanup = Cleanup(command.spawn().unwrap(), hwnd);
            let end = Instant::now() + Duration::from_secs(10);
            while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
                assert!(Instant::now() < end);
                std::thread::sleep(Duration::from_millis(20));
            }
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            pump_until(|| tray_exists(hwnd));
            assert_tray_menu([true, false, false, true]);
            ShowWindow(hwnd, SW_MINIMIZE);
            assert_eq!(IsWindowVisible(hwnd), 0);
            // Simulate Explorer losing its icon table, without restarting Explorer.
            Shell_NotifyIconW(NIM_DELETE, &tray_identity(hwnd));
            let recreated = *TASKBAR_CREATED.get().unwrap();
            assert_ne!(recreated, 0);
            SendMessageW(hwnd, recreated, 0, 0);
            pump_until(|| tray_exists(hwnd));
            SendMessageW(hwnd, TRAY_EVENT, 0, ((TRAY_ID << 16) | NIN_SELECT) as isize);
            assert_ne!(IsWindowVisible(hwnd), 0);
            assert_eq!(IsIconic(hwnd), 0);
            UI.with(|slot| {
                let slot = slot.borrow();
                let ui = slot.as_ref().unwrap();
                assert_ne!(IsWindowEnabled(ui.connect), 0);
                assert_eq!(IsWindowEnabled(ui.disconnect), 0);
            });
            SendMessageW(hwnd, WM_COMMAND, CONNECT, 0);
            pump_until(|| {
                UI.with(|slot| slot.borrow().as_ref().unwrap().status_value == Status::Connected)
            });
            UI.with(|slot| {
                let slot = slot.borrow();
                let ui = slot.as_ref().unwrap();
                assert!(text(ui.status).contains("隧道已连接"));
                assert!(text(ui.log).contains("tunnel ready"));
                assert_eq!(IsWindowEnabled(ui.combo), 0);
            });
            assert_tray_menu([false, true, true, true]);
            let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".tools");
            std::fs::create_dir_all(&out).unwrap();
            capture(hwnd, &out.join("gui-connected.bmp"));
            assert_ne!(
                GetClassLongPtrW(hwnd, GCLP_HICON),
                0,
                "embedded logo was not loaded"
            );
            UI.with(|slot| {
                let mut slot = slot.borrow_mut();
                let ui = slot.as_mut().unwrap();
                assert_ne!(IsWindowEnabled(ui.view), 0);
                assert_eq!(
                    xxtab::config::Config::load(&ui.store.path(0).unwrap())
                        .unwrap()
                        .remote,
                    "127.0.0.1:7007"
                );
                ui.command(VIEW).unwrap();
            });
            let viewer = EDITOR.with(|slot| {
                let mut slot = slot.borrow_mut();
                let editor = slot.as_mut().unwrap();
                assert!(editor.read_only);
                for field in [editor.name, editor.tunnel, editor.wg] {
                    assert_ne!(GetWindowLongW(field, GWL_STYLE) & ES_READONLY, 0);
                }
                assert_eq!(IsWindowVisible(editor.save), 0);
                assert_eq!(IsWindowVisible(editor.import), 0);
                assert!(
                    !editor.command(SAVE).unwrap(),
                    "read-only viewer must not save"
                );
                capture(editor.hwnd, &out.join("gui-viewer.bmp"));
                editor.hwnd
            });
            assert_tray_menu([false, false, false, false]);
            SendMessageW(viewer, WM_CLOSE, 0, 0);
            assert_ne!(IsWindowEnabled(hwnd), 0);
            assert!(UI.with(|slot| slot.borrow().as_ref().unwrap().worker.is_some()));
            ShowWindow(hwnd, SW_MINIMIZE);
            assert_eq!(IsWindowVisible(hwnd), 0);
            assert_ne!(SetTimer(hwnd, 77, 100, Some(dismiss_test_menu)), 0);
            show_tray_menu(hwnd);
            assert!(POPUP_PUMPED.with(Cell::get));
            assert!(!UI.with(|slot| slot.borrow().as_ref().unwrap().tray.menu_open));
            SendMessageW(hwnd, WM_COMMAND, RECONNECT, 0);
            assert_tray_menu([false, false, false, true]);
            pump_until(|| {
                UI.with(|slot| {
                    let slot = slot.borrow();
                    let ui = slot.as_ref().unwrap();
                    !ui.stopping && !ui.restart && ui.status_value == Status::Connected
                })
            });
            assert_eq!(IsWindowVisible(hwnd), 0);
            assert_tray_menu([false, true, true, true]);
            SendMessageW(hwnd, WM_COMMAND, DISCONNECT, 0);
            pump_until(|| UI.with(|slot| slot.borrow().as_ref().unwrap().worker.is_none()));
            assert_tray_menu([true, false, false, true]);
            SendMessageW(
                hwnd,
                TRAY_EVENT,
                0,
                ((TRAY_ID << 16) | TRAY_KEY_SELECT) as isize,
            );
            assert_ne!(IsWindowVisible(hwnd), 0);
            UI.with(|slot| slot.borrow_mut().as_mut().unwrap().command(EDIT).unwrap());
            assert_eq!(IsWindowEnabled(hwnd), 0);
            EDITOR.with(|slot| {
                let slot = slot.borrow();
                let editor = slot.as_ref().unwrap();
                assert!(!editor.changed());
                set_text(editor.name, "编辑后配置");
                capture(editor.hwnd, &out.join("gui-editor.bmp"));
            });
            let editor_hwnd = EDITOR.with(|slot| slot.borrow().as_ref().unwrap().hwnd);
            SendMessageW(editor_hwnd, WM_COMMAND, SAVE, 0);
            assert_ne!(IsWindowEnabled(hwnd), 0);
            assert!(EDITOR.with(|slot| slot.borrow().is_none()));
            UI.with(|slot| {
                let mut slot = slot.borrow_mut();
                let ui = slot.as_mut().unwrap();
                assert_eq!(ui.store.catalog.profiles[0].name, "编辑后配置");
                assert_ne!(IsWindowEnabled(ui.connect), 0);
                ui.command(CONNECT).unwrap();
            });
            pump_until(|| {
                UI.with(|slot| slot.borrow().as_ref().unwrap().status_value == Status::Connected)
            });
            ShowWindow(hwnd, SW_MINIMIZE);
            SendMessageW(hwnd, WM_COMMAND, EXIT, 0);
            pump_until(|| IsWindow(hwnd) == 0);
            assert!(!tray_exists(hwnd), "exit left a tray icon registered");
            let status = Command::new("sc.exe")
                .args(["query", "WireGuardTunnel$xxtaguitest"])
                .output()
                .unwrap();
            assert_eq!(
                status.status.code(),
                Some(1060),
                "GUI close left WireGuard service running"
            );
        }
    }
}
