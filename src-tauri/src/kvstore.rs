//! 本地键值存储：SQLite + 内存缓存。
//!
//! ## 为什么这么做
//!
//! 原来前端用 `localStorage` 存快照，底层是 Chromium 的 LevelDB ——
//! 单文件、无事务、看不到内容、清缓存就没了。
//!
//! 现在换成 SQLite，但**前端代码一行不改**：
//! 启动时把 SQLite 里的所有键值读进 Rust 的内存 map，
//! 前端调 `kv_list` 一次性拿全，
//! 之后所有读写走内存（同步、零延迟），
//! 由前端补一层 localStorage 兼容包装（见 `frontend/kv-bridge.js`）。
//!
//! 写入策略：前端每次 `setItem` 攒一小会儿（防抖），
//! 批量调 `kv_set_many` 落盘。SQLite 用 WAL + 事务，写入很快。
//!
//! ## 表结构
//!
//! ```sql
//! CREATE TABLE kv (
//!   k TEXT PRIMARY KEY,   -- 键名，和原来的 localStorage 键完全一致
//!   v TEXT NOT NULL,      -- 值（字符串，保持 localStorage 的语义）
//!   at INTEGER NOT NULL   -- 最后写入时间（毫秒）
//! );
//! ```
//!
//! 另外每次写入都往 `kv_history` 留一份，方便出问题时回滚。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use rusqlite::{params, Connection};

/// 全局连接。Tauri 的命令都在主线程/异步线程上跑，
/// 用 Mutex 包一层最简单，写入量很小不会成为瓶颈。
static DB: OnceLock<Mutex<Connection>> = OnceLock::new();
static DB_PATH: OnceLock<PathBuf> = OnceLock::new();

/// 打开（或创建）数据库，建表。在 setup 里调一次。
pub fn init(dir: PathBuf) -> Result<(), String> {
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("worktable.db");

    let conn = Connection::open(&path).map_err(|e| format!("打开数据库失败: {e}"))?;

    /* WAL 模式：写入不阻塞读取，单机小库最合适。
       synchronous=NORMAL 在 WAL 下是安全的，比默认快很多。 */
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         PRAGMA temp_store=MEMORY;",
    )
    .map_err(|e| e.to_string())?;

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS kv (
            k  TEXT PRIMARY KEY,
            v  TEXT NOT NULL,
            at INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS kv_history (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            k  TEXT NOT NULL,
            v  TEXT NOT NULL,
            at INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_kv_history_at ON kv_history(at);",
    )
    .map_err(|e| e.to_string())?;

    DB.set(Mutex::new(conn)).map_err(|_| "数据库已初始化".to_string())?;
    DB_PATH.set(path).map_err(|_| "路径已设置".to_string())?;
    Ok(())
}

/// 数据库文件路径（调试/备份用）
pub fn path() -> Option<String> {
    DB_PATH.get().map(|p| p.to_string_lossy().to_string())
}

fn with_db<T>(f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T, String> {
    let m = DB.get().ok_or_else(|| "数据库未初始化".to_string())?;
    let conn = m.lock().unwrap_or_else(|e| e.into_inner());
    f(&conn).map_err(|e| e.to_string())
}

/* ================= IPC 命令 ================= */

/// 读出全部键值。前端启动时调一次，之后用内存缓存。
#[tauri::command]
pub fn kv_all() -> Result<HashMap<String, String>, String> {
    with_db(|conn| {
        let mut stmt = conn.prepare("SELECT k, v FROM kv")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = HashMap::new();
        for it in rows {
            let (k, v) = it?;
            out.insert(k, v);
        }
        Ok(out)
    })
}

/// 单键读取（前端补漏用，正常情况直接用内存缓存）
#[tauri::command]
pub fn kv_get(key: String) -> Result<Option<String>, String> {
    with_db(|conn| {
        let mut stmt = conn.prepare("SELECT v FROM kv WHERE k = ?1")?;
        let mut rows = stmt.query_map([&key], |r| r.get::<_, String>(0))?;
        match rows.next() {
            Some(v) => Ok(Some(v?)),
            None => Ok(None),
        }
    })
}

/// 批量写入。前端防抖后把攒下的改动一次性发过来。
///
/// `pairs` 是 `[key, value]` 的数组；`value` 传空字符串表示"删除这个键"。
/// 用一个事务写完，比逐条写快一个数量级。
#[tauri::command]
pub fn kv_set_many(pairs: Vec<(String, String)>) -> Result<usize, String> {
    if pairs.is_empty() {
        return Ok(0);
    }
    with_db(|conn| {
        let tx = conn.unchecked_transaction()?;
        let now = now_ms();
        {
            let mut up = tx.prepare(
                "INSERT INTO kv (k, v, at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(k) DO UPDATE SET v = excluded.v, at = excluded.at",
            )?;
            let mut del = tx.prepare("DELETE FROM kv WHERE k = ?1")?;
            let mut hist =
                tx.prepare("INSERT INTO kv_history (k, v, at) VALUES (?1, ?2, ?3)")?;
            for (k, v) in &pairs {
                if v.is_empty() {
                    del.execute([k])?;
                } else {
                    up.execute(params![k, v, now])?;
                    /* 只给"大件"留历史（快照/图谱），小配置不留，免得库膨胀 */
                    if v.len() > 2000 && k.starts_with("dsh.worktable.") {
                        hist.execute(params![k, v, now])?;
                    }
                }
            }
        }
        tx.commit()?;
        /* 历史只留最近 200 条，超了删旧的 */
        let _ = conn.execute(
            "DELETE FROM kv_history WHERE id NOT IN
             (SELECT id FROM kv_history ORDER BY id DESC LIMIT 200)",
            [],
        );
        Ok(pairs.len())
    })
}

/// 单键写入（调试/迁移用）
#[tauri::command]
pub fn kv_set(key: String, value: String) -> Result<(), String> {
    kv_set_many(vec![(key, value)]).map(|_| ())
}

/// 数据库信息：路径、键数、文件大小。设置页显示用。
#[tauri::command]
pub fn kv_info() -> Result<serde_json::Value, String> {
    let (count, total) = with_db(|conn| {
        let count: i64 = conn.query_row("SELECT count(*) FROM kv", [], |r| r.get(0))?;
        let total: i64 = conn.query_row(
            "SELECT coalesce(sum(length(v)), 0) FROM kv",
            [],
            |r| r.get(0),
        )?;
        Ok((count, total))
    })?;

    let file_bytes = DB_PATH
        .get()
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .unwrap_or(0);

    Ok(serde_json::json!({
        "path": path().unwrap_or_default(),
        "keys": count,
        "dataBytes": total,
        "fileBytes": file_bytes,
    }))
}

/// 从 localStorage 迁移：前端把现有的键值整体发过来，只写库里没有的键。
/// 这样第一次升级时不会丢数据，也不会覆盖已有的。
#[tauri::command]
pub fn kv_import(pairs: Vec<(String, String)>) -> Result<usize, String> {
    let existing: HashMap<String, String> = kv_all()?;
    let todo: Vec<(String, String)> = pairs
        .into_iter()
        .filter(|(k, v)| !existing.contains_key(k) && !v.is_empty())
        .collect();
    kv_set_many(todo)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
