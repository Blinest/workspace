//! 本地服务托管（Python + Node）
//!
//! 所有服务脚本统一放在 exe 同级的 `services/` 目录（也兼容绿色版 `scripts/`）。
//!
//!   * 3920  scrapling_fetch.py  —— 网页正文抓取（剪藏助手）
//!   * 3921  zhihu_bridge.py     —— 知乎搜索 / 热榜（经 zhihu-cli）
//!   * 3923  invoice_parser.py   —— 发票 PDF 解析（发票管家）
//!   * 3918  regen-server.js     —— 周报总结 / 番茄钟周报的数据服务
//!
//! 这些服务也可以不从工作台启动 —— 在服务器上单独跑（见 services/README.md），
//! 工作台检测到端口已监听就会跳过启动，直接用现成的。
//!
//! 脚本路径解析顺序（发布版和开发版共用一套逻辑）：
//!   1. exe 同级的 `services/` 目录（安装包里的位置）
//!   2. exe 同级的 `scripts/` 目录（绿色版）
//!   3. 开发期的项目目录（target/release 上溯 → 项目根 + services/ 或 scripts/）
//!   4. 历史硬编码的 dsdata 目录（本机老环境兜底）
//!
//! 日志写在 %LOCALAPPDATA%\com.dsw.siyuan-research-worktable\services.log，
//! 这样即使做成无控制台的窗口程序也能事后排查。

use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// 不弹出黑色控制台窗口
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 老环境里的脚本目录（只作为最后一档兜底）
const LEGACY_DSDATA: &str = "";   // 留空 = 不使用老路径兜底

struct Service {
    name: &'static str,
    port: u16,
    /// 脚本文件名
    script: &'static str,
    /// 脚本从哪个子目录找（"services" 或 "dsdata"）
    subdir: &'static str,
    /// 拉起用的可执行文件，None = python / python3 / py 依次尝试
    command: Option<&'static str>,
}

const SERVICES: [Service; 2] = [
    /* 剪藏抓取服务。依赖较重（scrapling + playwright + Chromium），
       没装依赖时启动会失败 —— 但那只是记一条日志，不影响其它功能。
       前端「剪藏助手」在它不可用时会自动回落到 r.jina.ai。 */
    Service { name: "scrapling_fetch", port: 3920, script: "scrapling_fetch.py", subdir: "services", command: None },
    Service { name: "zhihu_bridge", port: 3921, script: "zhihu_bridge.py", subdir: "services", command: None },
    /* ⚠ 不在这里启动的服务：
       · 3918 周报  —— 本进程内的 Rust 实现，见 weekly.rs
       · 3923 发票  —— 作为「拓展服务」跑在独立服务器上（含 OCR，依赖大），
                       地址在界面「设置 → 拓展服务」里配置。
       两者都不随安装包分发脚本。 */
];

static CHILDREN: Mutex<Vec<(&'static str, Child)>> = Mutex::new(Vec::new());
/// 应用正在退出；看门狗据此停止补进程，否则 stop_all 之后又会被拉起来
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

fn port_open(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok()
}

/// 依次尝试 python / python3 / py，避免 PATH 里只有其中一个
fn exe_dir() -> PathBuf {
    let mut p = std::env::current_exe().unwrap_or_default();
    p.pop(); // 去掉 exe 文件名
    p
}

/// 按优先级返回候选脚本目录。
fn candidate_dirs(subdir: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let base = exe_dir();

    // 1) 安装包：exe 同级 services/
    out.push(base.join(subdir));
    // 2) 绿色版：exe 同级 scripts/
    out.push(base.join("scripts"));
    // 3) 开发期：<项目根>/scripts  （exe 在 src-tauri/target/debug/）
    let mut dev = base.clone();
    for _ in 0..3 {
        dev.pop();
    }
    out.push(dev.join("scripts"));
    // 4) 老环境兜底
    out.push(PathBuf::from(LEGACY_DSDATA));

    out.dedup();
    out
}

/// 开发期的 scripts/ 目录（保留旧函数名，供别处引用）
#[allow(dead_code)]
fn workspace_scripts() -> std::path::PathBuf {
    let mut p = exe_dir();
    p.pop(); // debug
    p.pop(); // target -> src-tauri
    p.pop(); // -> 项目根
    p.push("scripts");
    p
}

fn spawn_python(script: &Path, cwd: &Path) -> std::io::Result<Child> {
    let mut last: Option<std::io::Error> = None;
    for exe in ["python", "python3", "py"] {
        let mut cmd = Command::new(exe);
        cmd.arg(script)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);

        match cmd.spawn() {
            Ok(child) => return Ok(child),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "找不到 python 可执行文件")
    }))
}

fn spawn_node(script: &Path, cwd: &Path) -> std::io::Result<Child> {
    let mut cmd = Command::new("node");
    cmd.arg(script)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.spawn()
}

fn start_one(s: &Service) -> String {
    if port_open(s.port) {
        return format!("{}: 端口 {} 已在监听，跳过启动", s.name, s.port);
    }

    /* 按优先级找脚本：安装包 services/ → 绿色版 scripts/ → 开发期 scripts/ → 老环境 dsdata/。
       找到哪个就用哪个，并且把它的**所在目录**作为工作目录
       （周报服务靠 __dirname 定位 capacity.json，cwd 必须一致）。 */
    let mut found: Option<PathBuf> = None;
    for dir in candidate_dirs(s.subdir) {
        let cand = dir.join(s.script);
        if cand.is_file() {
            found = Some(cand);
            break;
        }
    }
    let script = match found {
        Some(p) => p,
        None => {
            let tried = candidate_dirs(s.subdir)
                .into_iter()
                .map(|d| d.display().to_string())
                .collect::<Vec<_>>()
                .join(" | ");
            return format!("{}: 脚本不存在 {}（已尝试: {}）", s.name, s.script, tried);
        }
    };
    let cwd = script.parent().map(|p| p.to_path_buf()).unwrap_or_else(exe_dir);

    let spawned = match s.command {
        Some(exe) => match exe {
            "node" => spawn_node(&script, &cwd),
            other => {
                let mut cmd = Command::new(other);
                cmd.arg(&script)
                    .current_dir(&cwd)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                #[cfg(windows)]
                cmd.creation_flags(CREATE_NO_WINDOW);
                cmd.spawn()
            }
        },
        None => spawn_python(&script, &cwd),
    };

    match spawned {
        Ok(child) => {
            let pid = child.id();
            if let Ok(mut v) = CHILDREN.lock() {
                v.push((s.name, child));
            }
            format!("{}: 已启动 pid {} (端口 {})", s.name, pid, s.port)
        }
        Err(e) => format!("{}: 启动失败 {}", s.name, e),
    }
}

pub fn start_all() -> Vec<String> {
    // 注意：不能在 setup() 里长时间等待就绪，否则窗口要等到超时才出现。
    // 这里只负责发起启动并记录结果；真实就绪状态由 services_status 实时探测。

    // 周报服务是本进程内的 Rust 实现（不需要 Node），单独起一个线程。
    // 它自己会探测 3918；已在监听就跳过，避免和工作台重复起两份。
    crate::weekly::spawn();

    let out: Vec<String> = SERVICES.iter().map(start_one).collect();
    write_log(&out);
    out
}

/// 后台看门狗：定期探端口，发现某个服务掉线就单独拉起来。
///
/// 管辖范围 = 本应用拉起的服务 ∪ 观察期内已经在跑的服务。
/// 后者的意义是接管上次没随窗口退出的残留进程：只要它此刻活着，
/// 之后掉线就该由我们补上。观察期里就已经是死的不补，
/// 避免和用户自己手动启动的进程打架。
pub fn spawn_watchdog() {
    std::thread::spawn(move || {
        // 首次给一点时间让 setup 里的 start_all 完成
        std::thread::sleep(Duration::from_secs(10));
        let mut managed = launched();
        for s in SERVICES.iter() {
            if port_open(s.port) && !managed.contains(&s.name) {
                managed.push(s.name);
            }
        }
        write_log(&[format!("[watchdog] 开始监视: {}", managed.join(", "))]);

        loop {
            if SHUTTING_DOWN.load(Ordering::Relaxed) {
                return;
            }
            for s in SERVICES.iter() {
                if port_open(s.port) || !managed.contains(&s.name) {
                    continue;
                }
                // 之前活着，现在掉线了 -> 重启
                reap_dead();
                let msg = start_one(s);
                write_log(&[format!("[watchdog] 检测到 {} 掉线，重新拉起: {}", s.name, msg)]);
            }
            std::thread::sleep(Duration::from_secs(20));
        }
    });
}

/// 本应用拉起过的服务名（CHILDREN 里记着的）
fn launched() -> Vec<&'static str> {
    CHILDREN
        .lock()
        .map(|v| v.iter().map(|(name, _)| *name).collect())
        .unwrap_or_default()
}

/// 回收已经自己退出的子进程，避免僵尸句柄里一直留着旧 pid
fn reap_dead() {
    if let Ok(mut v) = CHILDREN.lock() {
        let mut keep: Vec<(&'static str, Child)> = Vec::new();
        for (name, mut child) in v.drain(..) {
            match child.try_wait() {
                Ok(Some(_)) => eprintln!("[services] {} 已退出，清理句柄", name),
                _ => keep.push((name, child)),
            }
        }
        *v = keep;
    }
}

pub fn stop_all() {
    SHUTTING_DOWN.store(true, Ordering::Relaxed);
    if let Ok(mut v) = CHILDREN.lock() {
        for (name, child) in v.iter_mut() {
            let _ = child.kill();
            let _ = child.wait();
            eprintln!("[services] 已停止 {}", name);
        }
        v.clear();
    }
}

fn log_path() -> PathBuf {
    match std::env::var("LOCALAPPDATA") {
        Ok(p) => PathBuf::from(p)
            .join("com.dsw.siyuan-research-worktable")
            .join("services.log"),
        Err(_) => std::env::temp_dir().join("siyuan-worktable-services.log"),
    }
}

pub fn write_log(lines: &[String]) {
    let path = log_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "[{}] {}", secs, lines.join(" | "));
    }
}

#[tauri::command]
pub fn services_status() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    /* 周报服务在进程内，单独报告 */
    let weekly_state = if port_open(crate::weekly::PORT) { "已就绪" } else { "未运行" };
    out.push(format!("weekly: {} (端口 {})", weekly_state, crate::weekly::PORT));
    out.extend(SERVICES.iter().map(|s| {
        let state = if port_open(s.port) { "已就绪" } else { "未运行" };
        format!("{}: {} (端口 {})", s.name, state, s.port)
    }));
    out
}

#[tauri::command]
pub fn services_restart() -> Vec<String> {
    stop_all();
    // 这是重启而不是退出，让看门狗继续正常工作
    SHUTTING_DOWN.store(false, Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(400));
    start_all()
}
