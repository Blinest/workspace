//! 拖放支持：Tauri 2 的 WebView2 会拦截系统文件拖放并交给 Rust。
//! 这里把拖入的路径转成 base64 通过事件发给前端，由前端决定怎么处理。

use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};

static LAST_DROP: Mutex<Vec<String>> = Mutex::new(Vec::new());
static DRAG_ENTER: Mutex<bool> = Mutex::new(false);

/// 保存最近一次拖入的文件路径，并广播给前端
pub fn handle_drop(app: &AppHandle, paths: Vec<String>) {
    if let Ok(mut v) = DRAG_ENTER.lock() { *v = false; }
    if let Ok(mut v) = LAST_DROP.lock() {
        *v = paths.clone();
    }
    let _ = app.emit("drag-drop-files", paths);
}

/// 查询当前是否处于拖入状态（供前端轮询做高亮）
#[tauri::command]
pub fn drag_state() -> bool {
    DRAG_ENTER.lock().map(|v| *v).unwrap_or(false)
}

/// 前端读取最近一次拖入的路径（兜底方式）
#[tauri::command]
pub fn take_dropped_files() -> Vec<String> {
    if let Ok(mut v) = LAST_DROP.lock() {
        let out = v.clone();
        v.clear();
        out
    } else {
        Vec::new()
    }
}

/// 读取本地文件为 base64，供前端构造 File 对象上传
#[tauri::command]
pub fn read_file_base64(path: String) -> Result<String, String> {
    use std::io::Read;
    let mut f = std::fs::File::open(&path).map_err(|e| format!("打开失败: {}", e))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).map_err(|e| format!("读取失败: {}", e))?;
    Ok(base64_encode(&buf))
}

/// 简单 base64 编码（避免引入额外依赖）
fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
        out.push(CHARS[((n >> 18) & 63) as usize] as char);
        out.push(CHARS[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { CHARS[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { CHARS[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// 把拖放事件挂到主窗口上
pub fn attach(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let h = app.clone();
        win.on_window_event(move |ev| {
            if let tauri::WindowEvent::DragDrop(dd) = ev {
                match dd {
                    tauri::DragDropEvent::Enter { paths, .. } => {
                        if let Ok(mut v) = DRAG_ENTER.lock() { *v = true; }
                        let ps: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
                        let _ = h.emit("drag-drop-enter", ps);
                    }
                    tauri::DragDropEvent::Over { .. } => {}
                    tauri::DragDropEvent::Drop { paths, .. } => {
                        let ps: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
                        handle_drop(&h, ps);
                    }
                    tauri::DragDropEvent::Leave => {
                        if let Ok(mut v) = DRAG_ENTER.lock() { *v = false; }
                        let _ = h.emit("drag-drop-leave", ());
                    }
                    _ => {}
                }
            }
        });
    }
}
