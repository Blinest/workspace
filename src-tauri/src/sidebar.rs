//! 桌面侧边栏：无边框置顶窗口 + 全局热键呼出 + 拖动吸附。
//!
//! 全局热键这里没有引入 tauri-plugin-global-shortcut，而是直接调用 Win32
//! `RegisterHotKey`：本机 cargo 拉不到新 crate，而这件事用系统 API 就够了。
//! 做法是在独立线程里注册热键并跑一个消息循环，收到 WM_HOTKEY 就切换窗口显隐。
//!
//! 双屏：窗口可以自由拖动，松手后吸附到「离它最近的那块屏」的左右边缘，
//! 并把落点记进配置文件。下次启动/呼出就回到上次的位置，
//! 不会再被 primary_monitor 强行钉回主屏。

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;

use tauri::{AppHandle, LogicalSize, Manager, Monitor, PhysicalPosition, PhysicalSize, WebviewUrl,
            WebviewWindow, WebviewWindowBuilder};

pub const LABEL: &str = "sidebar";
/// 主工作台窗口的 label（对应 tauri.conf.json 里 windows[0].label）
pub const MAIN_LABEL: &str = "main";
/// 主窗口默认尺寸（逻辑像素）。**不最大化**：
/// 大窗口会盖住常驻的侧边栏，也会让「平时只看侧边栏」失去意义。
const MAIN_W: f64 = 1100.0;
const MAIN_H: f64 = 820.0;
/// 侧边栏宽度（逻辑像素）
const WIDTH: f64 = 400.0;
const HOTKEY_ID: i32 = 0xB001;

const MOD_ALT: u32 = 0x0001;
const MOD_CONTROL: u32 = 0x0002;
const MOD_SHIFT: u32 = 0x0004;
const MOD_WIN: u32 = 0x0008;
const VK_SPACE: u32 = 0x20;
const WM_HOTKEY: u32 = 0x0312;

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Point {
    x: i32,
    y: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Msg {
    hwnd: *mut c_void,
    message: u32,
    w_param: usize,
    l_param: isize,
    time: u32,
    pt: Point,
}

#[link(name = "user32")]
extern "system" {
    fn RegisterHotKey(hwnd: *mut c_void, id: i32, fs_modifiers: u32, vk: u32) -> i32;
    fn UnregisterHotKey(hwnd: *mut c_void, id: i32) -> i32;
    fn GetMessageW(msg: *mut Msg, hwnd: *mut c_void, min: u32, max: u32) -> i32;
    fn TranslateMessage(msg: *const Msg) -> i32;
    fn DispatchMessageW(msg: *const Msg) -> isize;
    fn GetWindowLongW(hwnd: *mut c_void, index: i32) -> i32;
    fn SetWindowLongW(hwnd: *mut c_void, index: i32, value: i32) -> i32;
    fn PostThreadMessageW(thread_id: u32, msg: u32, w_param: usize, l_param: isize) -> i32;
    /* 工作区：屏幕矩形减去任务栏等系统占用 */
    fn SystemParametersInfoW(action: u32, param: u32, pv: *mut RECT, win_ini: u32) -> i32;
    fn MonitorFromWindow(hwnd: *mut c_void, flags: u32) -> *mut c_void;
    fn GetMonitorInfoW(hmon: *mut c_void, lpmi: *mut MonitorInfo) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentThreadId() -> u32;
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct RECT {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MonitorInfo {
    cb_size: u32,
    rc_monitor: RECT,
    rc_work: RECT,
    flags: u32,
}

const SPI_GETWORKAREA: u32 = 0x0030;
const MONITOR_DEFAULTTONEAREST: u32 = 2;

/* ---------------- 毛玻璃背景 ---------------- */

#[repr(C)]
struct AccentPolicy {
    accent_state: i32,
    accent_flags: i32,
    gradient_color: u32,
    animation_id: i32,
}

#[repr(C)]
struct WinCompAttrData {
    attrib: i32,
    data: *mut c_void,
    size: usize,
}

const WCA_ACCENT_POLICY: i32 = 19;
/// 亚克力（Win10 1803+ / Win11），背后内容被实时虚化
#[allow(dead_code)]
const ACCENT_ENABLE_ACRYLICBLURBEHIND: i32 = 4;
/// 普通模糊：比亚克力轻，背后文字还能看清轮廓，更适合这种阅读型侧边栏
const ACCENT_ENABLE_BLURBEHIND: i32 = 3;
/// 完全透明，不做模糊（只要透出背景用这个）
const ACCENT_ENABLE_TRANSPARENTGRADIENT: i32 = 2;
const GWL_EXSTYLE: i32 = -20;
const WS_EX_LAYERED: i32 = 0x0008_0000;

/* SetWindowCompositionAttribute 没有导出符号，得从 user32 里动态取地址 */
#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleA(name: *const u8) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
}

type SetWcaFn = unsafe extern "system" fn(*mut c_void, *mut WinCompAttrData) -> i32;

/// 让 WebView 自己的背景透明。
///
/// 窗口设了 transparent(true) 之后，透出来的是 WebView2 的默认底色
/// （偏蓝的那块），不是桌面。必须再把这层也清掉。
fn clear_webview_bg(win: &WebviewWindow) {
    let _ = win.set_background_color(Some(tauri::window::Color(0, 0, 0, 0)));
}

#[link(name = "user32")]
extern "system" {
    /// 启用 WS_EX_LAYERED 后必须调用它，否则窗口被系统用默认色（蓝）填充
    fn SetLayeredWindowAttributes(hwnd: *mut c_void, key: u32, alpha: u8, flags: u32) -> i32;
}

const LWA_ALPHA: u32 = 0x0000_0002;

/// 给窗口加毛玻璃。
///
/// 关键顺序（之前漏了第 2 步导致整窗变蓝）：
///   1. 加 WS_EX_LAYERED
///   2. **必须**调 SetLayeredWindowAttributes(LWA_ALPHA, 255) 把整体不透明度设满
///   3. 再用 SetWindowCompositionAttribute 开启 ACRYLIC/BLURBEHIND
///
/// 不加第 2 步，系统会用默认色填充整个分层窗口（就是那块蓝色）。
fn apply_blur(win: &WebviewWindow) {
    let Ok(hwnd) = win.hwnd() else { return };
    let h = hwnd.0 as *mut c_void;

    unsafe {
        let user32 = GetModuleHandleA(b"user32.dll\0".as_ptr());
        if user32.is_null() {
            return;
        }
        let proc = GetProcAddress(user32, b"SetWindowCompositionAttribute\0".as_ptr());
        if proc.is_null() {
            eprintln!("[sidebar] blur 不可用（系统太旧）");
            return;
        }
        let set_wca: SetWcaFn = std::mem::transmute(proc);

        /* 1. 分层窗口 */
        let ex = GetWindowLongW(h, GWL_EXSTYLE);
        SetWindowLongW(h, GWL_EXSTYLE, ex | WS_EX_LAYERED);
        /* 2. 把整层不透明度设成 255（= 不额外变淡），
              这一步不做的话窗口会被默认色填充。 */
        let ok2 = SetLayeredWindowAttributes(h, 0, 255, LWA_ALPHA);
        if ok2 == 0 {
            eprintln!("[sidebar] SetLayeredWindowAttributes 失败");
        }

        /* 3. 亚克力：背景实时虚化，配上前面那层淡黄着色就是毛玻璃 */
        let mut policy = AccentPolicy {
            accent_state: ACCENT_ENABLE_ACRYLICBLURBEHIND,
            accent_flags: 0,
            /* 极淡暖黄（AABBGGRR）：alpha 0x22，让玻璃带一点温度又不发灰 */
            gradient_color: 0x22FFF4DE,
            animation_id: 0,
        };
        let mut data = WinCompAttrData {
            attrib: WCA_ACCENT_POLICY,
            data: &mut policy as *mut _ as *mut c_void,
            size: std::mem::size_of::<AccentPolicy>(),
        };
        let ok = set_wca(h, &mut data);
        eprintln!("[sidebar] apply_blur: {}", if ok != 0 { "ok" } else { "failed" });
    }
}

/* ---------------- 位置记忆 ---------------- */

/// 记住上一次吸附的落点。`edge` 是 SNAP_LEFT / SNAP_RIGHT。
/// 这里不引入 serde（本机 cargo 拉不到新 crate），直接存一行简单文本：
/// `x y w h edge`
#[derive(Debug, Clone, Copy)]
pub struct SavedPos {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub edge: i32,
}

const SNAP_LEFT: i32 = -1;
const SNAP_RIGHT: i32 = 1;

fn state_file(app: &AppHandle) -> Option<std::path::PathBuf> {
    let dir = app.path().app_config_dir().ok()?;
    let _ = std::fs::create_dir_all(&dir);
    Some(dir.join("sidebar-pos.txt"))
}

fn load_saved(app: &AppHandle) -> Option<SavedPos> {
    let p = state_file(app)?;
    let txt = std::fs::read_to_string(p).ok()?;
    let t = txt.trim();
    if t.is_empty() {
        return None;
    }
    let parts: Vec<&str> = t.split_whitespace().collect();
    if parts.len() < 5 {
        return None;
    }
    let x: i32 = parts[0].parse().ok()?;
    let y: i32 = parts[1].parse().ok()?;
    let w: u32 = parts[2].parse().ok()?;
    let h: u32 = parts[3].parse().ok()?;
    /* 坏记录直接丢掉，按「没有记录」处理（回落到主屏右缘）。
       历史上写过 -32000 -32000 237 39 这种哨兵值，
       照着恢复会把窗口扔到屏幕外，看起来就像热键失灵。 */
    if x <= -30000 || x >= 30000 || y <= -30000 || y >= 30000 || w < 200 || h < 300 {
        eprintln!("[sidebar] 位置记录异常（x={} y={} w={} h={}），已忽略", x, y, w, h);
        return None;
    }
    Some(SavedPos {
        x,
        y,
        w,
        h,
        edge: parts[4].parse().ok()?,
    })
}

fn save_pos(app: &AppHandle, pos: SavedPos) {
    /* ⚠ 必须校验再存。
       窗口在「还没显示 / 刚创建」时，outer_position 会返回 Win32 的哨兵值
       （-21333 或 -32000，即 CW_USEDEFAULT / 隐藏窗口坐标），
       尺寸也可能还没上屏（比如 237x39）。
       这些值一旦写进 sidebar-pos.txt，下次启动就会照着它把窗口摆到屏幕外 ——
       表现就是「按 Ctrl+Alt+Space 没反应」，其实窗口已经 show 了，
       只是尺寸 158x26、位置 -21333，谁看不见它。
       所以：坐标必须是合理范围、尺寸必须像样，不满足就不覆盖旧记录。 */
    let sane = pos.x > -30000 && pos.x < 30000
        && pos.y > -30000 && pos.y < 30000
        && pos.w >= 200 && pos.h >= 300;
    if !sane {
        eprintln!(
            "[sidebar] 拒绝保存异常位置 x={} y={} w={} h={}（窗口可能还没上屏）",
            pos.x, pos.y, pos.w, pos.h
        );
        return;
    }
    if let Some(p) = state_file(app) {
        let txt = format!("{} {} {} {} {}", pos.x, pos.y, pos.w, pos.h, pos.edge);
        let _ = std::fs::write(p, txt);
    }
}

/* ---------------- 吸附计算 ---------------- */


/// 找「窗口面积占比最大」的那块屏。
///
/// 不能用中心点判断：窗口跨在两屏之间时，中心点会在边界附近反复摇摆，
/// 导致吸附目标在左右两块屏之间跳，表现出来就是抖动。
/// 按重叠面积算，越过边界一半才换屏，结果稳定。
fn nearest_monitor(win: &WebviewWindow) -> tauri::Result<Option<Monitor>> {
    let pos = win.outer_position()?;
    let size = win.outer_size()?;
    let wx0 = pos.x as i64;
    let wy0 = pos.y as i64;
    let wx1 = wx0 + size.width as i64;
    let wy1 = wy0 + size.height as i64;

    let mons = win.available_monitors()?;
    if mons.is_empty() {
        return win.primary_monitor();
    }

    let mut best: Option<(i64, Monitor)> = None;
    for m in mons {
        let mp = m.position();
        let ms = m.size();
        let mx0 = mp.x as i64;
        let my0 = mp.y as i64;
        let mx1 = mx0 + ms.width as i64;
        let my1 = my0 + ms.height as i64;

        // 与窗口的重叠面积
        let ox = (wx1.min(mx1) - wx0.max(mx0)).max(0);
        let oy = (wy1.min(my1) - wy0.max(my0)).max(0);
        let area = ox * oy;

        if best.as_ref().map(|(ba, _)| area > *ba).unwrap_or(true) {
            best = Some((area, m));
        }
    }
    Ok(best.map(|(_, m)| m))
}

/// 把窗口吸到最近屏的某一侧（左/右），返回落点。
/// `content_w` 是前端报上来的真实内容宽度（物理像素），
/// 传 0 表示未知、退回用 outer_size。
/// 取窗口所在那块屏的「工作区」矩形（物理像素）：
/// 屏幕矩形减去任务栏等系统占用。任务栏隐藏时它就等于整屏。
fn work_area_of(win: &WebviewWindow) -> Option<RECT> {
    use raw_window_handle::HasWindowHandle;
    let h = win.window_handle().ok()?;
    let hwnd = match h.as_raw() {
        raw_window_handle::RawWindowHandle::Win32(w) => w.hwnd.get() as *mut c_void,
        _ => return None,
    };
    unsafe {
        let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        if hmon.is_null() { return None; }
        let mut mi = MonitorInfo {
            cb_size: std::mem::size_of::<MonitorInfo>() as u32,
            rc_monitor: RECT::default(),
            rc_work: RECT::default(),
            flags: 0,
        };
        if GetMonitorInfoW(hmon, &mut mi) == 0 { return None; }
        Some(mi.rc_work)
    }
}

/// 兜底：取主屏工作区（拿不到具体窗口时用）
fn primary_work_area() -> Option<RECT> {
    unsafe {
        let mut r = RECT::default();
        if SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut r, 0) == 0 { return None; }
        Some(r)
    }
}

fn snap_to_edge(app: &AppHandle, win: &WebviewWindow, content_w: u32) -> tauri::Result<Option<SavedPos>> {
    let Some(mon) = nearest_monitor(win)? else { return Ok(None) };
    let msize = mon.size();
    let mpos = mon.position();

    let cur = win.outer_position()?;
    let cur_size = win.outer_size()?;
    /* 外框比内容宽一圈不可见边框。实测：外 622、内容 600，多出 22 = 左右各 11。
       Windows 的 set_position 定的是**外框左上角**，所以要让看得见的内容贴住边缘，
       左边要减掉 left_border，右边要减掉 right_border。 */
    let inner_size = win.inner_size()?;
    let border = cur_size.width.saturating_sub(inner_size.width);
    let left_border = border / 2;
    let right_border = border - left_border;
    let visible_w = if content_w > 0 { content_w } else { inner_size.width };

    let center_x = cur.x as i64 + visible_w as i64 / 2;
    let mon_left = mpos.x as i64;
    let mon_right = mpos.x as i64 + msize.width as i64;

    // 离哪边近就吸哪边
    let edge = if (center_x - mon_left).abs() <= (mon_right - center_x).abs() {
        SNAP_LEFT
    } else {
        SNAP_RIGHT
    };
    /* 贴着看得见的内容：
       左缘 = 屏左界 - 左边框（把不可见边框推到屏外）
       右缘 = 屏右界 - 内容宽 - 右边框（同样把边框推出屏外） */
    let x = if edge == SNAP_LEFT {
        mon_left as i32 - left_border as i32
    } else {
        (mon_right - visible_w as i64 - right_border as i64) as i32
    };

    /* ⚠ 高度以「工作区」为准，不是整屏高度：
       工作区 = 屏幕 - 任务栏。任务栏隐藏时就等于整屏，
       任务栏在下面/上面会自动少掉对应的高度。
       以前完全不设高度，用的是窗口拖动后剩下的那个值，
       所以每次拖动都会留下不同的高度。 */
    let wa = work_area_of(win)
        .or_else(primary_work_area)
        .map(|r| (r.top, r.bottom))
        .unwrap_or((mpos.y, mpos.y + msize.height as i32));
    let (wa_top, wa_bottom) = wa;

    /* y 从工作区顶部开始，贴到底 */
    let y = wa_top;
    let want_h = (wa_bottom - wa_top).max(1) as u32;

    /* 外框比内容高一点（不可见边框），补上去让「看得见的部分」正好铺满 */
    let dh = cur_size.height.saturating_sub(inner_size.height);
    let h = want_h.saturating_sub(dh).max(1);

    /* 只在尺寸差得明显时才 set_size，避免反复调用把外框越改越大 */
    if (cur_size.height as i32 - want_h as i32).abs() > 4 && cur_size.width == inner_size.width + border {
        let _ = win.set_size(PhysicalSize::new(visible_w, h));
    }

    /* 吸附动画：从当前位置缓动到目标点。
       直接 set_position 会「啪」地跳过去，很生硬。
       这里用 18 帧、每帧约 11ms（合计约 200ms）做 cubic ease-out 插值，
       位移越大感觉越顺，位移小于 6px 就直接跳（看不出差别）。 */
    let dx = x - cur.x;
    let dy = y - cur.y;
    if dx.abs() > 6 || dy.abs() > 6 {
        const FRAMES: i32 = 18;
        for i in 1..=FRAMES {
            let t = i as f64 / FRAMES as f64;
            /* ease-out cubic：起步快、收尾慢，像被吸过去 */
            let e = 1.0 - (1.0 - t).powi(3);
            let nx = cur.x + (dx as f64 * e).round() as i32;
            let ny = cur.y + (dy as f64 * e).round() as i32;
            win.set_position(PhysicalPosition::new(nx, ny))?;
            std::thread::sleep(std::time::Duration::from_millis(11));
        }
    } else if dx != 0 || dy != 0 {
        win.set_position(PhysicalPosition::new(x, y))?;
    }
    let after_p = win.outer_position()?;
    let after_s = win.outer_size()?;
    let saved = SavedPos { x: after_p.x, y: after_p.y, w: after_s.width, h: after_s.height, edge };
    save_pos(app, saved);
    /* 贴边完成后，如果正在「占住屏幕边缘」，占位要跟着换到新的一边。
       不做的话会出现：窗口移到左边了，占位还留在右边 ——
       左边被压住、右边空一块，看起来就是「换边后别人不给我让位置」。 */
    crate::appbar::resync(app);
    Ok(Some(saved))
}

/// 恢复上次记住的位置；没有记录就贴主屏右缘。
///
/// 注意：双屏时「记住了扩展屏的坐标」本身没问题（用户可能就想放那儿），
/// 但**首次**（没有记录）必须落在主屏，否则会被 primary_monitor 的
/// 物理/逻辑换算带到别的屏上去。
/// 按记忆恢复位置：把高度对齐「落点所在的那块屏」，再定位。
/// 返回 false 表示没有记录或找不到对应的屏。
fn restore_saved(app: &AppHandle, win: &WebviewWindow) -> tauri::Result<bool> {
    let Some(s) = load_saved(app) else { return Ok(false) };
    let mons = win.available_monitors()?;
    let cx = s.x + s.w as i32 / 2;
    let cy = s.y + s.h as i32 / 2;
    for m in &mons {
        let mp = m.position();
        let ms = m.size();
        if cx >= mp.x && cx < mp.x + ms.width as i32
            && cy >= mp.y && cy < mp.y + ms.height as i32
        {
            /* ⚠ 单位必须统一，否则窗口会越开越窄。
               `save_pos` 存的是 outer_size()，那是**物理像素**；
               而 WebviewWindowBuilder::inner_size(WIDTH, 900) 用的是**逻辑像素**
               （内部会再乘 scale_factor）。
               以前这里直接把 s.w 当物理像素 set_size —— 在 150% 缩放下
               400 物理 = 267 逻辑，于是首次启动 400 宽、第二次启动变成 267 宽。
               现在：按记录所在屏的缩放，把物理宽度换算回逻辑宽度再设。 */
            let scale = m.scale_factor();
            let logical_w = if scale > 0.0 { (s.w as f64 / scale).round() } else { s.w as f64 };
            /* 宽度永远是设计宽度。
               以前允许「至少 WIDTH」，本意是防历史脏数据压窄；
               但预览功能曾经临时撑宽窗口并写进了记录，
               于是 748 这种宽度会被当成合法值一直恢复。
               现在统一用标准宽度 —— 侧边栏本来也不该被拉宽。 */
            let want_w = WIDTH;
            let _ = logical_w;

            /* w/h 记录的是「外框」，而 set_size 设的是「内容」。
               两者差一圈不可见边框（约 22x13）。先量当前差值再补偿，
               否则每次恢复都会缩一圈（1613→1453→1293…）。 */
            let cur_outer = win.outer_size()?;
            let cur_inner = win.inner_size()?;
            let dh = cur_outer.height.saturating_sub(cur_inner.height);
            let inner_h = ms.height.saturating_sub(dh);
            win.set_size(LogicalSize::new(want_w, inner_h as f64 / scale.max(0.1)))?;
            win.set_position(PhysicalPosition::new(s.x, mp.y))?;
            return Ok(true);
        }
    }
    /* 记录的那块屏不在了（拔掉了外接屏）：退回主屏右缘 */
    Ok(false)
}

/// 把窗口贴到主显示器右边缘并铺满高度。
///
/// 全部走「逻辑像素 → 用窗口自身 set_size/set_position 的物理值」这条路，
/// 而不是自己拿 scale_factor 乘，避免双屏不同缩放时算歪、窗口溢出到隔壁屏。
fn dock_right(win: &WebviewWindow) -> tauri::Result<()> {
    let Some(mon) = win.primary_monitor()? else { return Ok(()) };
    let scale = mon.scale_factor();
    let mpos = mon.position();          // 物理像素
    let msize = mon.size();             // 物理像素

    /* 高度以工作区为准（屏幕减任务栏），任务栏隐藏时等于整屏 */
    let wa = work_area_of(win).or_else(primary_work_area);
    let (wa_top, wa_bottom) = wa
        .map(|r| (r.top, r.bottom))
        .unwrap_or((mpos.y, mpos.y + msize.height as i32));
    let wa_h = (wa_bottom - wa_top).max(1) as u32;

    /* 只在尺寸明显不对时才 set_size（已对上就跳过），
       避免反复 set_size 让外框越改越大。 */
    let want_w = ((WIDTH * scale).round() as u32).min(msize.width);
    let cur = win.outer_size()?;
    let inner_now = win.inner_size()?;
    let dh = cur.height.saturating_sub(inner_now.height);
    let want_h = wa_h.saturating_sub(dh).max(1);
    if (cur.width as i32 - want_w as i32).abs() > 8
        || (cur.height as i32 - (want_h + dh) as i32).abs() > 8
    {
        win.set_size(PhysicalSize::new(want_w, want_h))?;
    }

    /* 定位按「看得见的内容」贴右缘：屏右界 - 内容宽 - 右边框。
       边框约 11px，不减掉就会留一道缝。 */
    let outer = win.outer_size()?;
    let inner = win.inner_size()?;
    let border = outer.width.saturating_sub(inner.width);
    let right_border = border - border / 2;
    let x = mpos.x + msize.width as i32 - inner.width as i32 - right_border as i32;
    let y = wa_top;
    win.set_position(PhysicalPosition::new(x, y))?;

    eprintln!(
        "[sidebar] dock_right: mon {}x{}@({},{}) scale={} inner={}x{} border={} -> pos=({},{})",
        msize.width, msize.height, mpos.x, mpos.y, scale,
        inner.width, inner.height, border, x, y
    );
    Ok(())
}

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    if app.get_webview_window(LABEL).is_some() {
        return Ok(());
    }
    let win = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("sidebar.html".into()))
        .title("工作台侧边栏")
        .inner_size(WIDTH, 900.0)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .visible(false)
        .focused(false)
        /* 透明：让前端画半透明玻璃层，系统模糊再把它背后的内容虚化 */
        .transparent(true)
        .shadow(false)
        .build()?;

    /* 毛玻璃：Windows 10/11 用 DWM 的 acrylic/blur 让背后内容虚化 */
    apply_blur(&win);

    /* WebView2 默认底色是 WebView 自带的（通常是偏蓝的默认色），
       窗口透明后这层就会透出来。必须把 WebView 自己的背景设成透明，
       否则看到的是那块蓝底而不是桌面。 */
    clear_webview_bg(&win);

    /* 恢复：记住的位置在哪块屏，就把高度对齐那块屏再定位，
       否则窗口会停在 inner_size 的 900 高。 */
    if restore_saved(app, &win)? {
        return Ok(());
    }
    dock_right(&win)?;
    /* 窗口刚创建时 outer_size 还是创建时的初始值，等它真正上屏了再读，
       否则会把「占位尺寸」记成用户的落点。宁可这次不记，也别记错。 */
    let app2 = app.clone();
    let win2 = win.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(900));
        if let (Ok(p), Ok(sz)) = (win2.outer_position(), win2.outer_size()) {
            save_pos(&app2, SavedPos { x: p.x, y: p.y, w: sz.width, h: sz.height, edge: SNAP_RIGHT });
        }
    });
    Ok(())
}

/// 切换侧边栏显隐；窗口不存在时顺手创建
pub fn toggle(app: &AppHandle) -> tauri::Result<()> {
    match app.get_webview_window(LABEL) {
        Some(win) => {
            if win.is_visible().unwrap_or(false) {
                /* ⚠ 隐藏前必须把 AppBar 占位一起撤掉。
                   只 hide 窗口的话，那块屏幕边缘仍然被「占着」——
                   其他应用最大化时会让出一条空白，看起来就是
                   「关了侧边栏，别的界面没恢复」。 */
                crate::appbar::unregister(&win);
                win.hide()?;
            } else {
                /* 呼出时保持用户上次拖到的地方，不要重置
                   （窗口一直存在，位置本来就没丢过） */
                win.show()?;
                /* 兜底：万一窗口被摆到了屏幕外（历史坏记录、拔掉副屏后
                   坐标失效等），用户会以为热键失灵。这里检查一下，
                   不在这块屏内就重新吸边摆回来。 */
                rescue_if_offscreen(app, &win);
                /* 如果用户开着「占住屏幕边缘」，重新占位 */
                crate::appbar::resync(app);
                win.set_focus()?;
            }
        }
        None => create(app)?,
    }
    Ok(())
}

/// 检查窗口是否落在某块屏幕的可视范围内；不在就重新吸边。
fn rescue_if_offscreen(app: &AppHandle, win: &WebviewWindow) {
    let Ok(pos) = win.outer_position() else { return };
    let Ok(size) = win.outer_size() else { return };
    let cx = pos.x + size.width as i32 / 2;
    let cy = pos.y + size.height as i32 / 2;

    let mut on_screen = false;
    if let Ok(mons) = win.available_monitors() {
        for m in &mons {
            let mp = m.position();
            let ms = m.size();
            if cx >= mp.x && cx < mp.x + ms.width as i32
                && cy >= mp.y && cy < mp.y + ms.height as i32
            {
                on_screen = true;
                break;
            }
        }
    }
    /* 尺寸也不对（比如 158x26）就一起修 */
    let bad_size = size.width < 200 || size.height < 300;

    if on_screen && !bad_size {
        return;
    }
    eprintln!("[sidebar] 窗口位置/尺寸异常（{:?} {:?}），重新吸边", pos, size);

    /* 先恢复成合理尺寸，再吸边 */
    if bad_size {
        let _ = win.set_size(PhysicalSize::new(400, 900));
    }
    let _ = snap_to_edge(app, win, 0);
    /* 顺手删掉那条坏记录，免得下次又按它恢复 */
    if let Some(p) = state_file(app) {
        let _ = std::fs::remove_file(p);
    }
}

/// 在独立线程注册 Ctrl+Alt+Space 并运行消息循环
/// 热键可以被用户在设置里改。
///
/// 实现方式：消息循环跑在一个线程里，另起一个监视线程看配置有没有变；
/// 变了就往热键线程投一条自定消息（`PostThreadMessageW`），
/// 由它自己完成 `UnregisterHotKey` + `RegisterHotKey` 换绑 ——
/// 热键绑定的是**线程**，必须在同一个线程里改。
static HOTKEY_MOD: AtomicU32 = AtomicU32::new(0);
static HOTKEY_VK: AtomicU32 = AtomicU32::new(0);
/// 热键消息循环线程的 ID（`PostThreadMessageW` 需要）
static HOTKEY_THREAD_ID: AtomicU32 = AtomicU32::new(0);
/// 换绑请求：`Some((mods, vk))`
static HOTKEY_WANT: Mutex<Option<(u32, u32)>> = Mutex::new(None);
/// 投递给热键线程的自定义消息号
const WM_APP_REBIND: u32 = 0x0400 + 1;

/// 热键配置文件名（放在 app_config_dir 下）
fn hotkey_file(app: &AppHandle) -> Option<std::path::PathBuf> {
    let dir = app.path().app_config_dir().ok()?;
    let _ = std::fs::create_dir_all(&dir);
    Some(dir.join("hotkey.txt"))
}

/// 从配置读热键，格式：`mods vk`（十进制）。
/// 读不到或格式不对就返回 None（调用方用默认值）。
fn load_hotkey(app: &AppHandle) -> Option<(u32, u32)> {
    let p = hotkey_file(app)?;
    let t = std::fs::read_to_string(p).ok()?;
    let mut it = t.split_whitespace();
    let m = it.next()?.parse::<u32>().ok()?;
    let v = it.next()?.parse::<u32>().ok()?;
    if m == 0 || v == 0 { return None; }
    Some((m, v))
}

fn save_hotkey(app: &AppHandle, mods: u32, vk: u32) {
    if let Some(p) = hotkey_file(app) {
        let _ = std::fs::write(p, format!("{} {}", mods, vk));
    }
}

/// 把修饰键 + 虚拟键翻译成 `Ctrl+Alt+Space` 这样的可读文本
fn hotkey_text(mods: u32, vk: u32) -> String {
    let mut s = String::new();
    if mods & MOD_CONTROL != 0 { s.push_str("Ctrl+"); }
    if mods & MOD_ALT != 0 { s.push_str("Alt+"); }
    if mods & MOD_SHIFT != 0 { s.push_str("Shift+"); }
    if mods & MOD_WIN != 0 { s.push_str("Win+"); }
    let key = match vk {
        0x20 => "Space".to_string(),
        0x0D => "Enter".to_string(),
        0x09 => "Tab".to_string(),
        0x1B => "Esc".to_string(),
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        0x30..=0x39 => ((vk as u8) as char).to_string(),
        0x41..=0x5A => ((vk as u8) as char).to_string(),
        _ => format!("VK{}", vk),
    };
    s.push_str(&key);
    s
}

/// 监视工作区变化（任务栏显示/隐藏、自动隐藏、分辨率变化等）。
///
/// 侧边栏的高度是「贴着工作区底部」的。任务栏一隐藏，工作区就变高，
/// 侧边栏却还停在原来的高度，底部会空出一条。这里轮询工作区矩形，
/// 变了就重新贴边，让高度自动跟上。
pub fn spawn_workarea_watcher(app: AppHandle) {
    let _ = std::thread::Builder::new()
        .name("sidebar-workarea".into())
        .spawn(move || {
            let mut last: Option<(i32, i32, i32, i32)> = None;
            let mut first = true;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(800));
                let Some(win) = app.get_webview_window(LABEL) else { continue };
                /* 窗口没显示就不用管 */
                if !win.is_visible().unwrap_or(false) {
                    last = None;
                    first = true;
                    continue;
                }
                let Some(wa) = work_area_of(&win).or_else(primary_work_area) else { continue };
                let cur = (wa.left, wa.top, wa.right, wa.bottom);

                /* 第一次只记下来，不重排 —— 避免启动时把用户拖好的位置改掉 */
                if first {
                    last = Some(cur);
                    first = false;
                    continue;
                }
                if last == Some(cur) { continue; }
                last = Some(cur);
                /* 工作区变了（任务栏显示/隐藏、分辨率变化）：重新贴边，
                   高度按新工作区重算 */
                let _ = snap_to_edge(&app, &win, 0);
            }
        });
}

/// 在独立线程注册热键并运行消息循环。
///
/// 默认 `Ctrl+Alt+Space`；用户在设置里改过就从配置读。
pub fn spawn_hotkey_thread(app: AppHandle) {
    let _ = std::thread::Builder::new()
        .name("sidebar-hotkey".into())
        .spawn(move || unsafe {
            HOTKEY_THREAD_ID.store(GetCurrentThreadId(), Ordering::Relaxed);

            let (mut mods, mut vk) = load_hotkey(&app).unwrap_or((MOD_CONTROL | MOD_ALT, VK_SPACE));

            /* 注册失败就退回默认组合，避免用户改成被占用的键后彻底没热键 */
            loop {
                if RegisterHotKey(std::ptr::null_mut(), HOTKEY_ID, mods, vk) != 0 {
                    HOTKEY_MOD.store(mods, Ordering::Relaxed);
                    HOTKEY_VK.store(vk, Ordering::Relaxed);
                    eprintln!("[sidebar] 已注册热键 {}", hotkey_text(mods, vk));
                    break;
                }
                eprintln!("[sidebar] 注册热键 {} 失败（可能被占用）", hotkey_text(mods, vk));
                if (mods, vk) == (MOD_CONTROL | MOD_ALT, VK_SPACE) { return; }
                mods = MOD_CONTROL | MOD_ALT;
                vk = VK_SPACE;
            }

            /* 监视线程：发现配置变了就往热键线程投一条自定消息 */
            let _ = std::thread::Builder::new()
                .name("sidebar-hotkey-watch".into())
                .spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_millis(600));
                    let want = { *HOTKEY_WANT.lock().unwrap() };
                    if let Some((m2, v2)) = want {
                        if (m2, v2) == (HOTKEY_MOD.load(Ordering::Relaxed), HOTKEY_VK.load(Ordering::Relaxed)) {
                            *HOTKEY_WANT.lock().unwrap() = None;
                            continue;
                        }
                        let tid = HOTKEY_THREAD_ID.load(Ordering::Relaxed);
                        if tid != 0 {
                            PostThreadMessageW(tid, WM_APP_REBIND, 0, 0);
                        }
                    }
                });

            let mut msg: Msg = std::mem::zeroed();
            loop {
                let r = GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
                if r <= 0 { break; }

                if msg.message == WM_APP_REBIND {
                    let want = { *HOTKEY_WANT.lock().unwrap() };
                    if let Some((m2, v2)) = want {
                        UnregisterHotKey(std::ptr::null_mut(), HOTKEY_ID);
                        if RegisterHotKey(std::ptr::null_mut(), HOTKEY_ID, m2, v2) != 0 {
                            HOTKEY_MOD.store(m2, Ordering::Relaxed);
                            HOTKEY_VK.store(v2, Ordering::Relaxed);
                            eprintln!("[sidebar] 热键已改为 {}", hotkey_text(m2, v2));
                        } else {
                            /* 新键被占用：把旧的注册回来 */
                            let om = HOTKEY_MOD.load(Ordering::Relaxed);
                            let ov = HOTKEY_VK.load(Ordering::Relaxed);
                            let _ = RegisterHotKey(std::ptr::null_mut(), HOTKEY_ID, om, ov);
                            eprintln!("[sidebar] 换绑 {} 失败，保持 {}",
                                      hotkey_text(m2, v2), hotkey_text(om, ov));
                        }
                        *HOTKEY_WANT.lock().unwrap() = None;
                    }
                    continue;
                }

                if msg.message == WM_HOTKEY {
                    let _ = toggle(&app);
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            UnregisterHotKey(std::ptr::null_mut(), HOTKEY_ID);
        });
}

/// 读取当前热键（给前端显示用）
#[tauri::command]
pub fn get_hotkey(app: AppHandle) -> Result<String, String> {
    let (m, v) = load_hotkey(&app).unwrap_or((MOD_CONTROL | MOD_ALT, VK_SPACE));
    Ok(hotkey_text(m, v))
}

/// 设置新热键。返回 `("ok", 显示文本)` 或 `("busy", 说明)`。
#[tauri::command]
pub fn set_hotkey(app: AppHandle, mods: u32, vk: u32) -> Result<String, String> {
    if mods == 0 { return Ok("none:至少要一个修饰键".into()); }
    if vk == 0 { return Ok("none:按键无效".into()); }
    save_hotkey(&app, mods, vk);
    *HOTKEY_WANT.lock().unwrap() = Some((mods, vk));
    Ok(format!("ok:{}", hotkey_text(mods, vk)))
}

/// 供前端/调试手动切换（侧边栏里按 Esc 也可以调它）
#[tauri::command]
pub fn toggle_sidebar(app: AppHandle) -> Result<(), String> {
    toggle(&app).map_err(|e| e.to_string())
}

/// 拖动结束后调用：吸到最近那块屏的左右边缘，并记住落点。
/// `contentW`：前端用 innerWidth * devicePixelRatio 算出的真实内容宽度（物理像素），
/// 用它贴右缘才准；不传则退回 outer_size。
#[tauri::command]
pub fn snap_sidebar(app: AppHandle, content_w: Option<u32>) -> Result<String, String> {
    let win = app
        .get_webview_window(LABEL)
        .ok_or_else(|| "侧边栏窗口不存在".to_string())?;
    let saved = snap_to_edge(&app, &win, content_w.unwrap_or(0))
        .map_err(|e| e.to_string())?;
    match saved {
        Some(s) => Ok(format!(
            "{{\"ok\":true,\"x\":{},\"y\":{},\"w\":{},\"h\":{},\"edge\":\"{}\"}}",
            s.x, s.y, s.w, s.h,
            if s.edge == SNAP_LEFT { "left" } else { "right" }
        )),
        None => Ok("{\"ok\":false}".to_string()),
    }
}

/// 预览窗口的 label
pub const PREVIEW_LABEL: &str = "preview";

/* ================= 预览窗口 =================
   侧边栏尺寸始终不变（400px），预览是**独立窗口**，贴在未贴附的一侧。

   为什么要专门开一个线程：
   Tauri 的 get_webview_window() / set_visible() 在 IPC 线程上会等
   窗口表的锁，而侧边栏窗口正被 WebView 持有 —— 一调就死锁。
   所以这里把「创建 / 隐藏」全部丢进一个常驻线程，用 channel 收命令，
   命令线程只负责发消息、立刻返回，绝不碰窗口 API。

   内容传递也不走 eval（会同步阻塞）和 URL（卡片 HTML 太长会白屏），
   而是写进 app_config_dir/preview-payload.json，页面自己读。
   ============================================ */

/// 预览窗口的工作线程命令
enum PvCmd {
    Show { html: String, title: String, x: i32, y: i32, w: i32, h: i32, on_right: bool },
    Hide,
    /// 定时轮询：鼠标是否还在预览窗里
    Poll,
    /// 只重读内容，不重建窗口
    Reload,
}

/// 发往工作线程的 sender（None = 线程还没起来）
static PV_TX: Mutex<Option<std::sync::mpsc::Sender<PvCmd>>> = Mutex::new(None);

/// 预览窗口 label 的序号。
///
/// ⚠ 每次显示都换一个新 label：
/// Tauri 2 在「同一个 label 反复 create/hide」时，内部注册表和窗口
/// 实际状态会不同步 —— get_webview_window 返回 None，但 build 报
/// "webview with label already exists"，之后再创建永远失败。
/// 每次换新名字可以彻底绕开；关掉旧窗口也不再有 label 冲突。
/// 光标是否停在预览窗里（由前端 preview_pointer 回报）
static PTR_INSIDE: AtomicBool = AtomicBool::new(false);
/// 上次收到「光标在窗里」心跳的时间（毫秒时间戳，0 = 从未）
static PTR_AT: AtomicU64 = AtomicU64::new(0);

/// 光标是否**真的**还在预览窗里：标记为 true 且 2 秒内有心跳。
/// 没有心跳说明预览窗那边没机会回报（鼠标直接拖出去了、
/// 或者压根没进来过），就当作已离开 —— 否则 Hide 会被永远挡住，
/// 浮层再也关不掉。
fn ptr_really_inside() -> bool {
    if !PTR_INSIDE.load(Ordering::SeqCst) {
        return false;
    }
    let at = PTR_AT.load(Ordering::SeqCst);
    if at == 0 {
        return false;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    now.saturating_sub(at) < 2000
}

/// 预览窗是否真的存在（前端用它校正 curCard）
static ALIVE: AtomicBool = AtomicBool::new(false);

static PV_SEQ: AtomicU32 = AtomicU32::new(0);


/// 鼠标当前是否落在给定矩形里（物理像素）
/// 取光标屏幕坐标（物理像素）。失败返回 None。
fn cursor_pos() -> Option<(i32, i32)> {
    #[repr(C)]
    struct Pt { x: i32, y: i32 }
    #[link(name = "user32")]
    extern "system" {
        fn GetCursorPos(p: *mut Pt) -> i32;
    }
    unsafe {
        let mut p = Pt { x: 0, y: 0 };
        if GetCursorPos(&mut p) == 0 { None } else { Some((p.x, p.y)) }
    }
}

/// 光标是否落在矩形内
fn point_in_rect(pt: (i32, i32), r: (i32, i32, i32, i32)) -> bool {
    let (l, t, rr, b) = r;
    pt.0 >= l && pt.0 <= rr && pt.1 >= t && pt.1 <= b
}

fn pointer_inside(rect: Option<(i32, i32, i32, i32)>) -> bool {
    let Some((l, t, r, b)) = rect else { return false };
    #[repr(C)]
    struct Pt { x: i32, y: i32 }
    #[link(name = "user32")]
    extern "system" {
        fn GetCursorPos(p: *mut Pt) -> i32;
    }
    unsafe {
        let mut p = Pt { x: 0, y: 0 };
        if GetCursorPos(&mut p) == 0 { return false; }
        p.x >= l && p.x <= r && p.y >= t && p.y <= b
    }
}
/// 启动预览窗口的工作线程。在 setup 里调一次即可。
pub fn spawn_preview_thread(app: AppHandle) {
    let (tx, rx) = std::sync::mpsc::channel::<PvCmd>();
    *PV_TX.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);

    let _ = std::thread::Builder::new()
        .name("preview-win".into())
        .spawn(move || {
            /* 所有窗口操作都在这个线程里做，不让 IPC 线程碰 */
            let mut cur: Option<(WebviewWindow, u32)> = None;

            /* 预览窗当前的屏幕矩形（物理像素），供「鼠标在里面就别关」判断 */
            let mut rect: Option<(i32, i32, i32, i32)> = None;
            /* 鼠标进过预览窗就置真 —— 之后即使离开卡片也保持显示，
               直到鼠标从预览窗移出才关 */
            let mut pinned = false;

            while let Ok(cmd) = rx.recv() {
                match cmd {
                    PvCmd::Hide => {
                        /* 关掉而不是隐藏：label 用完就丢，下次换新的。
                           但如果光标停在预览窗里，就先别关 —— 用户还要操作。

                           ⚠ 判断依据用 **前端回报的 PTR_INSIDE**，
                           不能用 pointer_inside(rect)：那个靠窗口矩形和
                           屏幕坐标去猜，在 124%~150% 缩放下经常误判成
                           "光标不在窗里"，于是在用户操作时把窗口关掉，
                           后续所有 preview_update 都打空 ——
                           表现就是「点了没反应」。 */
                        if ptr_really_inside() {
                            continue;
                        }
                        pinned = false;
                        rect = None;
                        if let Some((w, _)) = cur.take() {
                            let _ = w.destroy();
                        ALIVE.store(false, Ordering::SeqCst);
                        PTR_INSIDE.store(false, Ordering::SeqCst);
                        PTR_AT.store(0, Ordering::SeqCst);
                        }
                    }
                    PvCmd::Reload => {
                        /* 让预览窗重新读内容。
                           不 navigate（会重载页面、丢滚动和光标），
                           改为跑一段自愈脚本：
                             1. 先调 __reload()（重新读 payload 文件并渲染）
                             2. 顺便把面板状态也同步一次
                           这段在预览线程里执行，不占 IPC 线程。 */
                        if let Some((w, _)) = cur.as_ref() {
                            if w.is_visible().is_ok() {
                                let _ = w.eval(
                                    "try{                                        if(window.__reload) window.__reload();                                     }catch(e){}"
                                );
                            } else {
                                cur = None;
                                ALIVE.store(false, Ordering::SeqCst);
                        PTR_INSIDE.store(false, Ordering::SeqCst);
                        PTR_AT.store(0, Ordering::SeqCst);
                            }
                        }
                    }
                    PvCmd::Poll => {
                        /* 只维护「钉住」状态，**不主动销毁窗口**。
                           关闭完全交给前端的 preview_hide ——
                           那个判断基于 DOM 的 mouseover/mouseout，
                           不受 DPI 缩放和多屏坐标影响。 */
                        if cur.is_none() {
                            PTR_INSIDE.store(false, Ordering::SeqCst);
                            continue;
                        }
                        /* 按物理位置维护「光标在浮层里」标记。
                           这是前端判断"该不该关"的唯一可靠依据。 */
                        let inside = pointer_inside(rect);
                        PTR_INSIDE.store(inside, Ordering::SeqCst);
                        if inside {
                            pinned = true;
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as u64)
                                .unwrap_or(0);
                            PTR_AT.store(now, Ordering::SeqCst);
                        }
                    }
                    PvCmd::Show { html, title, x, y, w: sw, h, on_right } => {
                        /* 旧窗口先销毁 */
                        if let Some((w, _)) = cur.take() {
                            let _ = w.destroy();
                        ALIVE.store(false, Ordering::SeqCst);
                        PTR_INSIDE.store(false, Ordering::SeqCst);
                        PTR_AT.store(0, Ordering::SeqCst);
                            std::thread::sleep(std::time::Duration::from_millis(40));
                        }

                        /* 内容落盘，页面启动时自己读 */
                        if let Ok(dir) = app.path().app_config_dir() {
                            let _ = std::fs::create_dir_all(&dir);
                            let payload = serde_json::json!({ "title": title, "html": html });
                            if let Ok(txt) = serde_json::to_string(&payload) {
                                let _ = std::fs::write(dir.join("preview-payload.json"), txt);
                            }
                        }

                        /* 预览宽度比侧边栏宽 20%（400 → 480）。
                           侧边栏很窄时也保底 420，别挤成一列字。 */
                        /* pw_css：页面想要的 CSS 宽度（480）
                           pw_phys：传给窗口 API 的值，要 × sc ——
                           Tauri 对后建 webview 的尺寸按 scale_factor 折算，
                           实测传入 481 时页面只有 321（= 481/1.5）。 */
                        let pw_css: i32 = ((sw as f64) * 1.2).round().max(420.0) as i32;
                        let pw = pw_css;
                        let gap = 8;
                        let mut px = if on_right { x - pw - gap } else { x + sw + gap };

                        /* 自纠：跑出屏幕就翻到另一边 */
                        if let Ok(mons) = app.available_monitors() {
                            let mut inside = false;
                            let mut bl = i32::MIN;
                            let mut br = i32::MAX;
                            for m in mons {
                                let mp = m.position();
                                let ms = m.size();
                                let (ml, mr) = (mp.x, mp.x + ms.width as i32);
                                if px >= ml - pw && px <= mr {
                                    inside = true; bl = ml; br = mr; break;
                                }
                            }
                            if !inside || px < bl || px + pw > br {
                                px = if on_right { x - pw - gap } else { x + sw + gap };
                            }
                        }

                        /* 高度由前端按内容量出来（h 已经是物理像素）。
                           夹在 140~屏幕高-24 之间，避免一两条内容时太扁、
                           或者长列表顶出屏幕 */
                        let max_h = app
                            .available_monitors()
                            .ok()
                            .and_then(|ms| ms.into_iter().map(|m| m.size().height as i32).max())
                            .unwrap_or(1600)
                            - 24;
                        let h = h.max(140).min(max_h.max(160));

                        /* 顶部基准是卡片顶，但卡片往下滚时 y 可能超出屏幕
                           —— 夹一下，保证整个浮层都在屏幕内 */
                        let mon_top = app
                            .available_monitors()
                            .ok()
                            .and_then(|ms| ms.into_iter().map(|m| m.position().y).min())
                            .unwrap_or(0) as i32;
                        let y = (y as i32).max(mon_top + 8);
                        let y = y.min((mon_top + max_h - h).max(mon_top + 8));
                        /* 前端给的是物理像素，而 inner_size/position 用逻辑像素
                           —— 不分频的话 150% 缩放下窗口只有三分之二大 */
                        let sc = app.primary_monitor().ok().flatten().map(|m| m.scale_factor()).unwrap_or(1.0);
                        let sc = if sc > 0.1 { sc } else { 1.0 };
                        /* 页面想要的 CSS 宽度换算成窗口 API 需要的值 */
                        let pw_phys: i32 = ((pw_css as f64) * sc).round() as i32;
                        let seq = PV_SEQ.fetch_add(1, Ordering::Relaxed);
                        let label = format!("preview{}", seq);

                        match WebviewWindowBuilder::new(
                            &app, &label, WebviewUrl::App("preview.html".into())
                        )
                        .title("预览")
                        /* ⚠ 实测：这里要乘 sc，不是除。
                           除的话页面只剩 480/1.5 = 320 宽。 */
                        .inner_size(pw_phys as f64, h as f64)
                        .position(px as f64 / sc, y as f64 / sc)
                        .decorations(false)
                        .always_on_top(true)
                        .skip_taskbar(true)
                        .resizable(false)
                        .focused(false)
                        .visible(true)
                        .transparent(true)
                        .shadow(false)
                        .build()
                        {
                            Ok(w) => {
                                /* 用物理像素设一次，再稍后补一次 ——
                                   窗口刚建好时 webview 还没跟上 set_size，
                                   只设一次的话实际渲染宽度会停在 builder
                                   给的那个逻辑值上（480/1.5 = 320）。 */
                                let _ = w.set_position(PhysicalPosition::new(px, y));
                                let h_phys: i32 = h;
                                let _ = w.set_size(PhysicalSize::new(pw_phys as u32, h_phys as u32));
                                eprintln!("[preview] 已创建 {} @ ({},{}) {}x{}", label, px, y, pw, h);
                                let pw_phys = (pw as f64 * sc) as i32;
                                let h_phys = (h as f64 * sc) as i32;
                                rect = Some((px, y, px + pw_phys, y + h_phys));
                                pinned = false;
                                cur = Some((w, seq));
                    ALIVE.store(true, Ordering::SeqCst);
                    /* 新窗刚出来，光标大概率就在里面（悬停触发的） */
                    PTR_INSIDE.store(true, Ordering::SeqCst);
                            }
                            Err(e) => eprintln!("[preview] 创建 {} 失败: {}", label, e),
                        }
                    }
                }
            }
        });
}

/// 预览窗口里的点击/改动，转发给侧边栏去执行。
///
/// 预览里的 DOM 是从卡片拷贝出来的**副本**，原来绑的监听器不会跟过来。
/// 所以点一下就把动作原样发回去，由侧边栏在真实卡片上复现 ——
/// 数据改的是同一份，切回去能看到变化。
#[tauri::command]
pub fn preview_action(app: AppHandle, kind: String, data: serde_json::Value) {
    use tauri::Emitter;
    let payload = serde_json::json!({ "kind": kind, "data": data });
    let _ = app.emit_to(LABEL, "preview-action", payload);
}

/// 前端：轮询鼠标是否还在预览窗里
#[tauri::command]
pub fn preview_poll(_app: AppHandle) -> serde_json::Value {
    if let Some(t) = PV_TX.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        let _ = t.send(PvCmd::Poll);
    }
    /* 告诉前端两件事：
       1) 预览窗还在不在（用来校正 curCard）
       2) 光标是不是**物理上**在预览窗里 ——
          这个判断在 Rust 侧做，用 GetCursorPos + 窗口矩形，
          不受前端心跳时机影响，是「该不该关」最可靠的依据。 */
    let alive = ALIVE.load(Ordering::SeqCst);
    let ptr_in = alive && PTR_INSIDE.load(Ordering::SeqCst);
    serde_json::json!({ "alive": alive, "ptrIn": ptr_in })
}

/// 前端：显示预览
#[tauri::command]
pub fn preview_show(
    _app: AppHandle,
    html: String,
    title: String,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    on_right: bool,
) -> Result<(), String> {
    let tx = PV_TX.lock().unwrap_or_else(|e| e.into_inner()).clone();
    match tx {
        Some(t) => {
            let _ = t.send(PvCmd::Show { html, title, x, y, w, h, on_right });
            Ok(())
        }
        None => Err("预览线程未启动".into()),
    }
}

/// 前端：隐藏预览
#[tauri::command]
pub fn preview_hide(_app: AppHandle) -> Result<(), String> {
    let tx = PV_TX.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(t) = tx {
        let _ = t.send(PvCmd::Hide);
    }
    Ok(())
}

/// 前端：预览窗里的光标进出时通知侧边栏。
/// 侧边栏据此决定"能不能关窗口"—— 光标在里面时绝不能关。
#[tauri::command]
pub fn preview_pointer(app: AppHandle, inside: bool) {
    use tauri::Emitter;
    PTR_INSIDE.store(inside, Ordering::SeqCst);
    if inside {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        PTR_AT.store(now, Ordering::SeqCst);
    } else {
        PTR_AT.store(0, Ordering::SeqCst);
    }
    let _ = app.emit_to(LABEL, "preview-pointer", serde_json::json!({ "inside": inside }));
}

/// 只更新预览内容，不重建窗口。
///
/// 预览里点了勾选框之类，侧边栏改完数据后调这个 ——
/// 用 preview_show 会销毁重建窗口，用户正在浮层里操作会被打断。
#[tauri::command]
pub fn preview_update(app: AppHandle, html: String, title: String) -> Result<(), String> {
    /* 写文件，再通知预览窗重读 */
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    let _ = std::fs::create_dir_all(&dir);
    let payload = serde_json::json!({ "title": title, "html": html });
    let txt = serde_json::to_string(&payload).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("preview-payload.json"), txt).map_err(|e| e.to_string())?;

    /* 让预览窗重读（它在自己的线程里，安全） */
    let tx = PV_TX.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(t) = tx {
        let _ = t.send(PvCmd::Reload);
    }
    Ok(())
}

/// 读取预览内容（预览窗口启动时调用）
#[tauri::command]
pub fn preview_payload(app: AppHandle) -> Result<String, String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    std::fs::read_to_string(dir.join("preview-payload.json")).map_err(|e| e.to_string())
}

/// 查询当前吸附落点（调试用）
#[tauri::command]
pub fn sidebar_pos(app: AppHandle) -> Result<String, String> {
    let win = app
        .get_webview_window(LABEL)
        .ok_or_else(|| "侧边栏窗口不存在".to_string())?;
    let p = win.outer_position().map_err(|e| e.to_string())?;
    let s = win.outer_size().map_err(|e| e.to_string())?;
    Ok(format!(
        "{{\"x\":{},\"y\":{},\"w\":{},\"h\":{}}}",
        p.x, p.y, s.width, s.height
    ))
}

/// 把侧边栏移到指定物理坐标（前端拖动/脚本调试用）
#[tauri::command]
pub fn move_sidebar(app: AppHandle, x: i32, y: i32) -> Result<(), String> {
    let win = app
        .get_webview_window(LABEL)
        .ok_or_else(|| "侧边栏窗口不存在".to_string())?;
    win.set_position(PhysicalPosition::new(x, y))
        .map_err(|e| e.to_string())
}

/// 显示主窗口并聚焦（侧边栏的「⋯」用它跳回主界面）
/// 用系统默认程序打开 URL / 文件 / 自定义协议（siyuan://、file:// 等）
#[tauri::command]
pub fn open_url(target: String) -> Result<(), String> {
    if target.trim().is_empty() {
        return Err("空地址".into());
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        // 交给系统 shell 处理，支持 siyuan:// 协议、http(s)、本地文件
        std::process::Command::new("cmd")
            .args(["/C", "start", "", &target])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("打开失败: {}", e))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        std::process::Command::new(opener)
            .arg(&target)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("打开失败: {}", e))
    }
}

/// 从侧边栏打开主工作台。
/// **侧边栏保持显示**：平时只开侧边栏，需要完整工作台时点一下把它调出来。
///
/// 这里要处理两种情况：
///   ① 窗口还在（可能被最小化）→ unminimize + show + focus
///   ② 窗口已被用户关闭销毁 → **重建**
///
/// ⚠ 重建必须绕开命令线程：
/// `WebviewWindowBuilder::build()` 内部要等主线程事件循环把窗口建好，
/// 而 `run_on_main_thread` 的回调本身就跑在那条循环里，
/// 在回调里再 build 会把自己锁死（实测：窗口出现 but 是 about:blank，
/// 命令永远不返回）。所以这里用 `tauri::async_runtime::spawn` 起独立任务，
/// 让 build 在真正的异步上下文里完成。
#[tauri::command]
pub fn show_main(app: AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window(MAIN_LABEL) {
        /* 还存在：可能被最小化了，先还原再置前 */
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        avoid_sidebar(&app, &w);
        return Ok(());
    }
    /* 已被关闭销毁：在异步任务里重建，命令立即返回 */
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        /* 双重检查：投递期间可能已经被别的路径建好了 */
        if let Some(w) = app2.get_webview_window(MAIN_LABEL) {
            let _ = w.unminimize();
            let _ = w.show();
            let _ = w.set_focus();
            avoid_sidebar(&app2, &w);
            return;
        }
        /* URL 与侧边栏保持一致，写具体的文件名。
           **不最大化**：大窗口会盖住侧边栏，也让「侧边栏常驻」失去意义。 */
        match WebviewWindowBuilder::new(&app2, MAIN_LABEL, WebviewUrl::App("index.html".into()))
            .title("工作台")
            .inner_size(MAIN_W, MAIN_H)
            .min_inner_size(900.0, 600.0)
            .resizable(true)
            /* 无系统边框：最小化/最大化/关闭由页面顶部自己画，
               顶栏才能和纸感界面融为一体（深色系统条太割裂） */
            .decorations(false)
            .build()
        {
            Ok(w) => {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
                avoid_sidebar(&app2, &w);
                eprintln!("[main] 主窗口已重建");
            }
            Err(e) => eprintln!("[main] 重建主窗口失败: {}", e),
        }
    });
    Ok(())
}

/// 让主窗口落在「侧边栏没占住」的那块区域里。
///
/// 侧边栏固定在屏幕左缘或右缘（宽 400 逻辑像素，置顶不遮挡）。
/// 主窗口如果居中或最大化，就会压到侧边栏上。
/// 这里把主窗口放到另一侧，宽度缩到「屏幕宽 - 侧边栏宽」之内。
fn avoid_sidebar(app: &AppHandle, main: &WebviewWindow) {
    let side = match app.get_webview_window(LABEL) {
        Some(s) => s,
        None => return,
    };
    /* 侧边栏不可见就不用让位 */
    if !side.is_visible().unwrap_or(false) {
        return;
    }
    let sp = match side.outer_position() {
        Ok(p) => p,
        Err(_) => return,
    };
    let ss = match side.outer_size() {
        Ok(s) => s,
        Err(_) => return,
    };
    let mon = match side.current_monitor() {
        Ok(Some(m)) => m,
        _ => return,
    };
    let mp = mon.position();
    let ms = mon.size();

    let side_w = ss.width as i32;
    let side_center = sp.x + side_w / 2;
    let mon_center = mp.x + ms.width as i32 / 2;
    /* 侧边栏在左半屏 → 主窗口靠右；反之靠左 */
    let side_on_left = side_center < mon_center;
    let free_left = if side_on_left { mp.x + side_w } else { mp.x };
    let free_w = (ms.width as i32 - side_w).max(420);

    /* 主窗口宽度不超过可用区域。物理像素 = 逻辑 * 缩放 */
    let scale = main.scale_factor().unwrap_or(1.0);
    let cur = main.outer_size().unwrap_or(PhysicalSize::new(
        (MAIN_W * scale) as u32,
        (MAIN_H * scale) as u32,
    ));
    let want_w = (cur.width as i32).min(free_w).max((900.0 * scale) as i32);
    let want_h = (cur.height as i32).min(ms.height as i32 - 40).max((600.0 * scale) as i32);
    if cur.width as i32 != want_w || cur.height as i32 != want_h {
        let _ = main.set_size(PhysicalSize::new(want_w as u32, want_h as u32));
    }

    /* 居中于可用区域 */
    let x = free_left + (free_w - want_w) / 2;
    let y = mp.y + (ms.height as i32 - want_h) / 2;
    let _ = main.set_position(PhysicalPosition::new(x, y));
}

/// 主窗口当前是否可见（存在且 visible 且未最小化）。
/// 侧边栏用它来同步按钮的开关状态。
#[tauri::command]
pub fn main_visible(app: AppHandle) -> bool {    match app.get_webview_window(MAIN_LABEL) {
        Some(w) => {
            let shown = w.is_visible().unwrap_or(false);
            let minimized = w.is_minimized().unwrap_or(false);
            shown && !minimized
        }
        None => false,
    }
}

/// 切换主窗口显隐，返回切换后的可见状态。
/// 侧边栏的「工作台」按钮就是用它做开关：
///   关着 → 打开（窗口被销毁过就重建）
///   开着 → 隐藏（不销毁，下次打开更快，工作台里的状态也保留）
#[tauri::command]
pub fn toggle_main(app: AppHandle) -> Result<bool, String> {
    if let Some(w) = app.get_webview_window(MAIN_LABEL) {
        let shown = w.is_visible().unwrap_or(false);
        let minimized = w.is_minimized().unwrap_or(false);
        if shown && !minimized {
            /* 开着 → 收起来 */
            let _ = w.hide();
            return Ok(false);
        }
        /* 关着（或最小化）→ 显示出来 */
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        avoid_sidebar(&app, &w);
        return Ok(true);
    }
    /* 窗口已被关闭销毁：重建 */
    show_main(app)?;
    Ok(true)
}

/// 主窗口无系统边框后，最小化/最大化/关闭由页面自己的按钮调用这里。
/// `action`：minimize | maximize | close
#[tauri::command]
pub fn main_window_action(app: AppHandle, action: String) -> Result<(), String> {
    let win = app
        .get_webview_window(MAIN_LABEL)
        .ok_or_else(|| "主窗口不存在".to_string())?;
    match action.as_str() {
        "minimize" => win.minimize().map_err(|e| e.to_string()),
        "maximize" => {
            /* 已最大化则还原，否则最大化 —— 和系统按钮行为一致 */
            if win.is_maximized().unwrap_or(false) {
                win.unmaximize().map_err(|e| e.to_string())
            } else {
                win.maximize().map_err(|e| e.to_string())
            }
        }
        "close" => {
            /* 收起来而不是销毁：下次打开更快，工作台里的状态也留着 */
            win.hide().map_err(|e| e.to_string())
        }
        other => Err(format!("未知的窗口操作: {}", other)),
    }
}

/// 主窗口是否处于最大化（用于切换按钮图标）
#[tauri::command]
pub fn main_maximized(app: AppHandle) -> bool {
    app.get_webview_window(MAIN_LABEL)
        .map(|w| w.is_maximized().unwrap_or(false))
        .unwrap_or(false)
}