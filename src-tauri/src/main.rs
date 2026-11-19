// 思源科研工作台 · 桌面版外壳
// 前端资产（HTML/CSS）已被 Tauri 嵌进二进制，本文件只负责开一个原生窗口，
// 并在启动时托管本机依赖的后台服务（3921 / 3923 / 3918）。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod services;
mod sidebar;
mod siyuan;
mod dragdrop;
mod weekly;
mod appbar;

use tauri::Manager;

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            services::services_status,
            services::services_restart,
            siyuan::open_in_siyuan,
            sidebar::toggle_sidebar,
            sidebar::snap_sidebar,
            sidebar::sidebar_pos,
            sidebar::preview_show,
            sidebar::preview_hide,
            sidebar::preview_payload,
            sidebar::preview_pointer,
            sidebar::preview_update,
            sidebar::preview_poll,
            sidebar::preview_action,
            sidebar::move_sidebar,
            sidebar::show_main,
            sidebar::toggle_main,
            sidebar::main_visible,
            sidebar::main_window_action,
            sidebar::main_maximized,
            sidebar::open_url,
            sidebar::get_hotkey,
            sidebar::set_hotkey,
            appbar::appbar_set,
            dragdrop::take_dropped_files,
            dragdrop::read_file_base64,
            dragdrop::drag_state
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            // 预建侧边栏，并**开机直接显示**：
            // 平时只用侧边栏（待办/番茄钟/剪藏），需要完整工作台时
            // 点侧边栏顶部的按钮把主窗口调出来（主窗口默认 visible:false）。
            if let Err(e) = sidebar::create(&handle) {
                eprintln!("[sidebar] 创建侧边栏失败: {}", e);
            }
            if let Some(win) = handle.get_webview_window("sidebar") {
                let _ = win.show();
            }
            // 关闭主窗口时不让程序退出：侧边栏还在跑，
            // 之后还能再次打开工作台（show_main 会在窗口被销毁后重建它）。
            // Tauri 默认「所有窗口关闭才退出」，而侧边栏一直存在，
            // 所以这里其实不需要额外拦截，只记录一下便于排查。
            if let Some(main) = handle.get_webview_window("main") {
                main.on_window_event(|ev| {
                    if let tauri::WindowEvent::CloseRequested { .. } = ev {
                        eprintln!("[main] 主窗口被关闭，侧边栏保持运行");
                    }
                });
            }
            sidebar::spawn_hotkey_thread(handle.clone());
            /* 预览窗口的专用线程：所有建窗/隐藏都在这里做，
               避免 IPC 线程碰窗口 API 时死锁 */
            sidebar::spawn_preview_thread(handle.clone());
            /* 任务栏显示/隐藏会改变工作区高度，侧边栏要跟着长高/变矮 */
            sidebar::spawn_workarea_watcher(handle.clone());
            // 托管系统拖放（WebView2 会拦截 HTML 拖放事件）
            dragdrop::attach(&handle);
            // 后台拉起 3921 / 3923 / 3918；已在监听则跳过，失败只记日志不影响主窗口
            let _ = services::start_all();
            // 之后若某个服务意外退出，30s 内自动补起来
            services::spawn_watchdog();
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("思源科研工作台启动失败")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                /* 退出前撤掉 AppBar 占位，否则那条屏幕边缘会一直「被占着」，
                   别的应用最大化时留一条空白，直到重启资源管理器才恢复。 */
                if let Some(win) = app.get_webview_window(sidebar::LABEL) {
                    appbar::unregister(&win);
                }
                services::stop_all();
            }
        });
}
