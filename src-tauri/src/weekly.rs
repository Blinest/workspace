//! 周报数据服务（Rust 版，替代原来的 regen-server.js）
//!
//! 为什么重写：原服务需要用户机器上装 Node.js。工作台本身是 Rust 的，
//! 把这一层也搬到 Rust 里，安装包就不用再带一个 Node 运行时。
//!
//! 提供这些接口（端口 3918）：
//!   GET  /health      → {"ok":true,"pid":N}
//!   GET  /weekly      → 周报页 HTML（把模板里的 {{CAPACITY}} 换成真实数据）
//!   GET  /capacity    → 当前缓存的「每日执行力」数据
//!   POST /capacity    → 工作台推送「每日执行力」，存盘后供 /weekly 使用
//!   GET  /config      → 当前思源连接配置
//!   POST /config      → 修改思源连接配置
//!   POST /notes       → 写入 AI 生成的笔记总结区块
//!
//! 不提供的（原来依赖无头浏览器，发布版里去掉）：
//!   /pdf /png /regen —— 需要 Edge/Chrome 截图与外部 MCP，
//!   前端调用失败时会提示，不影响周报正文。

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

pub const PORT: u16 = 3918;

/// 模板与运行期文件的目录（exe 同级 services/，或开发期项目目录）
static ROOT: OnceLock<PathBuf> = OnceLock::new();
/// 每日执行力缓存
static CAPACITY: OnceLock<Mutex<String>> = OnceLock::new();
/// 思源连接配置
static CONFIG: OnceLock<Mutex<Config>> = OnceLock::new();

#[derive(Clone, Default)]
struct Config {
    api: String,
    token: String,
}

fn cfg() -> &'static Mutex<Config> {
    CONFIG.get_or_init(|| Mutex::new(Config::default()))
}

fn cap_state() -> &'static Mutex<String> {
    CAPACITY.get_or_init(|| Mutex::new(String::new()))
}

/// 候选目录：exe 同级 services/ → 同级 scripts/ → 开发期项目根 → 老环境
fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut base = std::env::current_exe().unwrap_or_default();
    base.pop();
    out.push(base.join("services"));
    out.push(base.join("scripts"));
    let mut dev = base.clone();
    for _ in 0..3 {
        dev.pop();
    }
    out.push(dev.join("services"));
    out.push(dev.join("scripts"));
    if let Ok(legacy) = std::env::var("DSH_LEGACY_DSDATA") {
        if !legacy.trim().is_empty() { out.push(PathBuf::from(legacy)); }
    }
    out.dedup();
    out
}

fn find_file(name: &str) -> Option<PathBuf> {
    for d in candidates() {
        let p = d.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn template_dir() -> PathBuf {
    if let Some(p) = find_file("weekly-summary.html") {
        if let Some(d) = p.parent() {
            return d.to_path_buf();
        }
    }
    candidates().into_iter().next().unwrap_or_default()
}

fn capacity_file() -> PathBuf {
    template_dir().join("capacity.json")
}

fn config_file() -> PathBuf {
    /* 用户配置目录优先 —— 安装包里不带私有 token 的配置 */
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let p = PathBuf::from(local)
            .join("com.dsw.siyuan-research-worktable")
            .join("regen-config.json");
        if p.is_file() {
            return p;
        }
        return p;
    }
    template_dir().join("regen-config.json")
}

fn load_config() {
    let mut c = Config::default();
    c.api = std::env::var("SIYUAN_API_URL").unwrap_or_else(|_| "http://127.0.0.1:6806".into());
    c.token = std::env::var("SIYUAN_TOKEN").unwrap_or_default();

    /* 文件覆盖环境变量的默认值（环境变量优先级最高的是「有值」时） */
    if let Ok(txt) = std::fs::read_to_string(config_file()) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
            if let Some(a) = v.get("api").and_then(|x| x.as_str()) {
                if !a.trim().is_empty() {
                    c.api = a.trim().trim_end_matches('/').to_string();
                }
            }
            if let Some(t) = v.get("token").and_then(|x| x.as_str()) {
                if !t.trim().is_empty() {
                    c.token = t.trim().to_string();
                }
            }
        }
    }
    if let Ok(mut g) = cfg().lock() {
        *g = c;
    }
}

fn load_capacity() -> String {
    if !cap_state().lock().map(|g| g.is_empty()).unwrap_or(true) {
        return cap_state().lock().map(|g| g.clone()).unwrap_or_default();
    }
    if let Ok(txt) = std::fs::read_to_string(capacity_file()) {
        if let Ok(mut g) = cap_state().lock() {
            *g = txt.clone();
        }
        return txt;
    }
    String::new()
}

fn json_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    for ch in s.chars() {
        match ch {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

/// 把 capacity.json 渲染成「每日执行力」区块的 HTML。
/// 与原来的 buildCapacityHtml 保持同样的结构与样式类名。
fn build_capacity_html(raw: &str) -> String {
    let v: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => return empty_capacity_panel(),
    };
    let days = match v.get("days").and_then(|d| d.as_array()) {
        Some(d) if !d.is_empty() => d,
        _ => return empty_capacity_panel(),
    };

    let num = |o: &serde_json::Value, k: &str| -> f64 {
        o.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0)
    };
    let mut rows = String::new();
    for d in days.iter().rev() {
        let score = num(d, "score");
        let cls = if score >= 80.0 {
            "g"
        } else if score >= 50.0 {
            "m"
        } else {
            "b"
        };
        let date = d.get("date").and_then(|x| x.as_str()).unwrap_or("");
        let short = if date.len() >= 10 { &date[5..10] } else { date };
        rows.push_str(&format!(
            "<div class=\"capRow\"><span class=\"capDate\">{}</span>\
             <span class=\"capBar\"><i class=\"{}\" style=\"width:{}%\"></i></span>\
             <span class=\"capNum\">{}/{}</span>\
             <span class=\"capMin\">{}分</span></div>",
            short,
            cls,
            score as i64,
            num(d, "done") as i64,
            num(d, "total") as i64,
            num(d, "mins") as i64
        ));
    }
    format!(
        "<div class=\"panel\"><div class=\"capHead\">近 {} 天 · 平均每天 {} 项 / {} 分钟 · 完成率 {}%</div>{}</div>",
        days.len(),
        num(&v, "avgTasks"),
        num(&v, "avgMins"),
        num(&v, "avgScore"),
        rows
    )
}

fn empty_capacity_panel() -> String {
    "<div class=\"panel\"><p style=\"color:#a39c8e;font-size:12px;margin:0\">还没有历史数据。每天的完成率与用时会在云端累积后显示。</p></div>".to_string()
}

/// 用真实数据替换模板里的 CAPACITY 区块
fn render_weekly() -> Result<String, String> {
    let path = find_file("weekly-summary.html")
        .ok_or_else(|| "找不到 weekly-summary.html 模板".to_string())?;
    let html = std::fs::read_to_string(&path).map_err(|e| format!("读取模板失败: {}", e))?;
    let cap = build_capacity_html(&load_capacity());

    let start = "<!--{{CAPACITY}}-->";
    let end = "<!--{{/CAPACITY}}-->";
    if let (Some(a), Some(b)) = (html.find(start), html.find(end)) {
        let mut out = String::with_capacity(html.len() + cap.len());
        out.push_str(&html[..a + start.len()]);
        out.push_str(&cap);
        out.push_str(&html[b..]);
        return Ok(out);
    }
    Ok(html)
}

fn send(stream: &mut TcpStream, code: u16, ctype: &str, body: &str) {
    let resp = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nAccess-Control-Allow-Origin: *\r\n\
         Connection: close\r\n\r\n{}",
        code,
        if code == 200 { "OK" } else { "Error" },
        ctype,
        body.as_bytes().len(),
        body
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
}

fn send_json(stream: &mut TcpStream, code: u16, body: &str) {
    send(stream, code, "application/json; charset=utf-8", body);
}

fn handle(mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut buf = [0u8; 65536];
    let n = match stream.read(&mut buf) {
        Ok(n) => n,
        Err(_) => return,
    };
    if n == 0 {
        return;
    }
    let raw = String::from_utf8_lossy(&buf[..n]).to_string();

    let mut lines = raw.split("\r\n");
    let first = lines.next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("GET");
    let url = parts.next().unwrap_or("/");

    /* body：空行之后的部分 */
    let body = if let Some(idx) = raw.find("\r\n\r\n") {
        raw[idx + 4..].to_string()
    } else {
        String::new()
    };

    let path = url.split('?').next().unwrap_or("/");

    match (method, path) {
        ("GET", "/health") => {
            let body = format!("{{\"ok\":true,\"pid\":{}}}", std::process::id());
            send_json(&mut stream, 200, &body);
        }
        ("GET", "/weekly") => match render_weekly() {
            Ok(html) => send(&mut stream, 200, "text/html; charset=utf-8", &html),
            Err(e) => send(&mut stream, 500, "text/plain; charset=utf-8", &e),
        },
        ("GET", "/capacity") => {
            let raw = load_capacity();
            let body = if raw.trim().is_empty() { "{}".to_string() } else { raw };
            send_json(&mut stream, 200, &body);
        }
        ("POST", "/capacity") => {
            if serde_json::from_str::<serde_json::Value>(&body).is_ok() {
                if let Ok(mut g) = cap_state().lock() {
                    *g = body.clone();
                }
                let _ = std::fs::write(capacity_file(), &body);
                send_json(&mut stream, 200, "{\"ok\":true}");
            } else {
                send_json(&mut stream, 400, "{\"ok\":false,\"error\":\"不是合法 JSON\"}");
            }
        }
        ("GET", "/config") => {
            let c = cfg().lock().map(|g| g.clone()).unwrap_or_default();
            let body = format!(
                "{{\"ok\":true,\"api\":\"{}\",\"hasToken\":{},\"configPath\":\"{}\"}}",
                json_escape(&c.api),
                !c.token.is_empty(),
                json_escape(&config_file().to_string_lossy())
            );
            send_json(&mut stream, 200, &body);
        }
        ("POST", "/config") => {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                let api = v.get("api").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
                let token = v.get("token").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
                if !api.starts_with("http://") && !api.starts_with("https://") {
                    send_json(&mut stream, 400, "{\"ok\":false,\"error\":\"api 必须以 http:// 或 https:// 开头\"}");
                    return;
                }
                let api = api.trim_end_matches('/').to_string();
                let path = config_file();
                if let Some(d) = path.parent() {
                    let _ = std::fs::create_dir_all(d);
                }
                let txt = format!(
                    "{{\n  \"api\": \"{}\",\n  \"token\": \"{}\"\n}}\n",
                    json_escape(&api),
                    json_escape(&token)
                );
                match std::fs::write(&path, txt) {
                    Ok(_) => {
                        if let Ok(mut g) = cfg().lock() {
                            g.api = api.clone();
                            g.token = token.clone();
                        }
                        send_json(&mut stream, 200,
                            &format!("{{\"ok\":true,\"api\":\"{}\",\"hasToken\":{}}}",
                                     json_escape(&api), !token.is_empty()));
                    }
                    Err(e) => send_json(&mut stream, 500,
                        &format!("{{\"ok\":false,\"error\":\"{}\"}}", json_escape(&e.to_string()))),
                }
            } else {
                send_json(&mut stream, 400, "{\"ok\":false,\"error\":\"请求体不是合法 JSON\"}");
            }
        }
        ("POST", "/notes") => {
            /* 把 AI 摘要写回 weekly-summary.html 的 NOTES 区块 */
            let path = match find_file("weekly-summary.html") {
                Some(p) => p,
                None => {
                    send_json(&mut stream, 500, "{\"ok\":false,\"error\":\"找不到模板\"}");
                    return;
                }
            };
            let html = match std::fs::read_to_string(&path) {
                Ok(h) => h,
                Err(e) => {
                    send_json(&mut stream, 500,
                        &format!("{{\"ok\":false,\"error\":\"{}\"}}", json_escape(&e.to_string())));
                    return;
                }
            };
            let s = "<!--{{NOTES}}-->";
            let e = "<!--{{/NOTES}}-->";
            if let (Some(a), Some(b)) = (html.find(s), html.find(e)) {
                if b > a {
                    let mut out = String::with_capacity(html.len() + body.len());
                    out.push_str(&html[..a + s.len()]);
                    out.push('\n');
                    out.push_str(&body);
                    out.push('\n');
                    out.push_str(&html[b..]);
                    match std::fs::write(&path, out) {
                        Ok(_) => send_json(&mut stream, 200, "{\"ok\":true}"),
                        Err(err) => send_json(&mut stream, 500,
                            &format!("{{\"ok\":false,\"error\":\"{}\"}}", json_escape(&err.to_string()))),
                    }
                } else {
                    send_json(&mut stream, 500, "{\"ok\":false,\"error\":\"NOTES 锚点顺序不对\"}");
                }
            } else {
                send_json(&mut stream, 500, "{\"ok\":false,\"error\":\"模板里找不到 {{NOTES}} 锚点\"}");
            }
        }
        _ => {
            send_json(&mut stream, 404, "{\"ok\":false,\"error\":\"not found\"}");
        }
    }
}

/// 启动周报服务（阻塞在独立线程里）。
/// 端口已被占用时直接返回，不报错 —— 说明别处已经起了一份。
pub fn start() {
    let addr = SocketAddr::from(([127, 0, 0, 1], PORT));
    if TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok() {
        eprintln!("[weekly] 端口 {} 已在监听，跳过启动", PORT);
        return;
    }
    let _ = ROOT.set(template_dir());
    load_config();
    load_capacity();

    let listener = match TcpListener::bind(addr) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[weekly] 绑定端口 {} 失败: {}", PORT, e);
            return;
        }
    };
    eprintln!("[weekly] Rust 周报服务已启动 http://127.0.0.1:{}", PORT);
    for stream in listener.incoming() {
        if let Ok(s) = stream {
            std::thread::spawn(move || handle(s));
        }
    }
}

/// 在后台线程里启动
pub fn spawn() {
    std::thread::spawn(|| start());
}
