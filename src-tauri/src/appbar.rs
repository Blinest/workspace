//! 把侧边栏注册成 Windows AppBar（像任务栏那样占用一条屏幕边缘）。
//!
//! 注册之后，其他应用「最大化」或「贴边对齐」时会自动避开这块区域，
//! 侧边栏就不会再遮住别人的界面。这是 Windows 官方提供的工作区保留机制
//! （`SHAppBarMessage`），对接的软件很广：资源管理器、Chrome、VS Code、
//! Office 等都支持。
//!
//! 局限（系统限制，不是本程序的 bug）：
//!   · 只对「最大化」和「贴屏幕边缘」的窗口生效
//!   · 普通大小、位置随意的窗口不会自动移开
//!   · 某些全屏游戏/播放器会忽略工作区，仍然覆盖

use std::ffi::c_void;
use std::sync::Mutex;

use tauri::{AppHandle, Manager, WebviewWindow};

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AppBarData {
    cb_size: u32,
    hwnd: *mut c_void,
    callback_message: u32,
    edge: u32,
    rect: Rect,
    l_param: isize,
}

const ABM_NEW: u32 = 0x0000_0000;
const ABM_REMOVE: u32 = 0x0000_0001;
const ABM_QUERYPOS: u32 = 0x0000_0002;
const ABM_SETPOS: u32 = 0x0000_0003;
const ABM_SETAUTOHIDEBAR: u32 = 0x0000_0008;

const ABE_LEFT: u32 = 0;
const ABE_TOP: u32 = 1;
const ABE_RIGHT: u32 = 2;
const ABE_BOTTOM: u32 = 3;

#[link(name = "shell32")]
extern "system" {
    fn SHAppBarMessage(dw_message: u32, p_data: *mut AppBarData) -> usize;
}

/// 当前是否已注册（避免重复注册）
static REGISTERED: Mutex<bool> = Mutex::new(false);
/// 用户是否开启了「占住屏幕边缘」（和 REGISTERED 分开：
/// 隐藏侧边栏时会临时撤掉占位，但用户的开关状态不变，
/// 再次呼出时要自动占回来）
static WANTED: Mutex<bool> = Mutex::new(false);
/// 当前注册用的是哪条边（换边时要先 REMOVE 再 NEW）
static CUR_EDGE: Mutex<u32> = Mutex::new(ABE_RIGHT);

fn is_wanted() -> bool {
    *WANTED.lock().unwrap_or_else(|e| e.into_inner())
}
fn set_wanted(v: bool) {
    *WANTED.lock().unwrap_or_else(|e| e.into_inner()) = v;
}

fn make_data(hwnd: *mut c_void, edge: u32, r: Rect) -> AppBarData {
    AppBarData {
        cb_size: std::mem::size_of::<AppBarData>() as u32,
        hwnd,
        callback_message: 0, // 不接收 ABN_* 回调，够用
        edge,
        rect: r,
        l_param: 0,
    }
}

/// 取得窗口的原始 HWND
fn hwnd_of(win: &WebviewWindow) -> Option<*mut c_void> {
    use raw_window_handle::HasWindowHandle;
    let h = win.window_handle().ok()?;
    match h.as_raw() {
        raw_window_handle::RawWindowHandle::Win32(w) => Some(w.hwnd.get() as *mut c_void),
        _ => None,
    }
}

/// 删除 Windows 标题栏的点击区域（透明窗口常见需求，这里顺带用不到）
fn _unused() {}

/// 把窗口所在的屏幕边缘登记为 AppBar 占位。
///
/// `edge` 传 `ABE_RIGHT` 或 `ABE_LEFT`；`rect` 是窗口的物理像素矩形。
///
/// ⚠ 换边（左 ↔ 右）时必须先 `ABM_REMOVE` 再 `ABM_NEW`：
/// 一个 hwnd 只能注册一条边，直接 NEW 会失败，表现为
/// 「贴到另一边后其他应用不再让位」。
pub fn register(win: &WebviewWindow, edge: u32, rect: Rect) -> bool {
    let Some(hwnd) = hwnd_of(win) else {
        eprintln!("[appbar] 拿不到 HWND，跳过注册");
        return false;
    };

    /* 已经注册过：如果边没变，只更新位置；变了就整体重来 */
    let was = *REGISTERED.lock().unwrap_or_else(|e| e.into_inner());
    if was {
        let old_edge = *CUR_EDGE.lock().unwrap_or_else(|e| e.into_inner());
        if old_edge == edge {
            update(win, edge, rect);
            return true;
        }
        unregister(win);
    }

    unsafe {
        let mut d = make_data(hwnd, edge, rect);
        let ok = SHAppBarMessage(ABM_NEW, &mut d);
        if ok == 0 {
            eprintln!("[appbar] ABM_NEW 失败");
            return false;
        }
        *REGISTERED.lock().unwrap_or_else(|e| e.into_inner()) = true;
        *CUR_EDGE.lock().unwrap_or_else(|e| e.into_inner()) = edge;

        /* 先问位置：系统会根据其它 AppBar/任务栏调整这个矩形 */
        let mut q = make_data(hwnd, edge, rect);
        SHAppBarMessage(ABM_QUERYPOS, &mut q);
        /* 宽度以我们自己为准（系统可能因为别家占位而压窄） */
        match edge {
            ABE_RIGHT => {
                q.rect.left = q.rect.right - (rect.right - rect.left);
            }
            ABE_LEFT => {
                q.rect.right = q.rect.left + (rect.right - rect.left);
            }
            _ => {}
        }
        let mut s = make_data(hwnd, edge, q.rect);
        SHAppBarMessage(ABM_SETPOS, &mut s);
        /* 按系统最终定下的位置摆窗口 */
        let _ = win.set_position(tauri::PhysicalPosition::new(s.rect.left, s.rect.top));
        let _ = win.set_size(tauri::PhysicalSize::new(
            (s.rect.right - s.rect.left).max(1) as u32,
            (s.rect.bottom - s.rect.top).max(1) as u32,
        ));
        eprintln!(
            "[appbar] 已注册 {} 侧占位：({}, {})-({}, {})",
            if edge == ABE_RIGHT { "右" } else { "左" },
            s.rect.left, s.rect.top, s.rect.right, s.rect.bottom
        );
        true
    }
}

/// 更新占位区域（窗口被拖动/改尺寸后调用）
pub fn update(win: &WebviewWindow, edge: u32, rect: Rect) {
    if !*REGISTERED.lock().unwrap_or_else(|e| e.into_inner()) {
        return;
    }
    /* 换边不算「更新」，交给 register 走完整流程 */
    let cur_edge = *CUR_EDGE.lock().unwrap_or_else(|e| e.into_inner());
    if cur_edge != edge {
        register(win, edge, rect);
        return;
    }
    let Some(hwnd) = hwnd_of(win) else { return };
    unsafe {
        let mut q = make_data(hwnd, edge, rect);
        SHAppBarMessage(ABM_QUERYPOS, &mut q);
        match edge {
            ABE_RIGHT => q.rect.left = q.rect.right - (rect.right - rect.left),
            ABE_LEFT => q.rect.right = q.rect.left + (rect.right - rect.left),
            _ => {}
        }
        let mut s = make_data(hwnd, edge, q.rect);
        SHAppBarMessage(ABM_SETPOS, &mut s);
        /* 窗口也摆到系统最终定下的位置，避免「占位在左边、窗口还在右边」 */
        let _ = win.set_position(tauri::PhysicalPosition::new(s.rect.left, s.rect.top));
        let _ = win.set_size(tauri::PhysicalSize::new(
            (s.rect.right - s.rect.left).max(1) as u32,
            (s.rect.bottom - s.rect.top).max(1) as u32,
        ));
    }
}

/// 取消占位（隐藏侧边栏、退出时调用）
pub fn unregister(win: &WebviewWindow) {
    let mut g = match REGISTERED.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    if !*g {
        return;
    }
    if let Some(hwnd) = hwnd_of(win) {
        unsafe {
            /* 用实际注册时的那条边去移除，否则系统对不上 */
            let e = *CUR_EDGE.lock().unwrap_or_else(|err| err.into_inner());
            let mut d = make_data(hwnd, e, Rect::default());
            SHAppBarMessage(ABM_REMOVE, &mut d);
            /* 顺便关掉自动隐藏标记 */
            let mut h = make_data(hwnd, e, Rect::default());
            SHAppBarMessage(ABM_SETAUTOHIDEBAR, &mut h);
        }
        eprintln!("[appbar] 已取消占位");
    }
    *g = false;
}

/// 前端/隐藏时用：只要用户开着「占住屏幕边缘」，就按窗口实际所在的边
/// 重新注册一次。贴到另一边、或从隐藏状态呼出后调用它，占位就能跟上。
pub fn resync(app: &AppHandle) {
    if !is_wanted() {
        return;
    }
    let Some(win) = app.get_webview_window(crate::sidebar::LABEL) else { return };
    /* 窗口不可见时不要占位 —— 那块空间应该还给其他应用 */
    if !win.is_visible().unwrap_or(false) {
        return;
    }
    if let Some(r) = window_rect(&win) {
        let e = edge_of(&win);
        register(&win, e, r);
    }
}

/// 当前窗口的物理矩形
pub fn window_rect(win: &WebviewWindow) -> Option<Rect> {
    let p = win.outer_position().ok()?;
    let s = win.outer_size().ok()?;
    Some(Rect {
        left: p.x,
        top: p.y,
        right: p.x + s.width as i32,
        bottom: p.y + s.height as i32,
    })
}

/// 判断窗口贴的是左边缘还是右边缘（用于决定 AppBar 的 edge）
pub fn edge_of(win: &WebviewWindow) -> u32 {
    if let Ok(Some(mon)) = win.current_monitor() {
        let mp = mon.position();
        let ms = mon.size();
        if let Some(r) = window_rect(win) {
            let c = mp.x + ms.width as i32 / 2;
            let wc = (r.left + r.right) / 2;
            return if wc >= c { ABE_RIGHT } else { ABE_LEFT };
        }
    }
    ABE_RIGHT
}

/// 给外部（命令）用：注册 / 取消
pub fn set_enabled(app: &AppHandle, on: bool) {
    set_wanted(on);
    let Some(win) = app.get_webview_window(crate::sidebar::LABEL) else { return };
    if on {
        if let Some(r) = window_rect(&win) {
            let e = edge_of(&win);
            register(&win, e, r);
        }
    } else {
        unregister(&win);
    }
}

/// 前端命令：开启 / 关闭「占住屏幕边缘」。
/// 返回当前状态（"on" / "off"），前端据此显示。
#[tauri::command]
pub fn appbar_set(app: AppHandle, on: bool) -> Result<String, String> {
    set_enabled(&app, on);
    Ok(if on { "on".into() } else { "off".into() })
}
