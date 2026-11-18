//! 与思源桌面端的联动：把文档「跳转打开」交给已注册的 siyuan:// 协议。
//!
//! 不做成 HTTP 调用，是因为「打开某个文档」属于 GUI 行为，思源没有对应的
//! Web API；而 SiYuan.exe 会自动注册 siyuan://blocks/<id> 这个协议，交给
//! 系统去唤起是最稳的方式。

use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 思源的块 ID 形如 20261104212014-1d54a6k：
/// 14 位日期 + '-' + 7 位 [a-z0-9]
fn is_block_id(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 22 || b[14] != b'-' {
        return false;
    }
    b[..14].iter().all(|c| c.is_ascii_digit())
        && b[15..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

/// 在思源里打开指定文档。返回 Ok 表示已成功唤起系统默认处理程序。
#[tauri::command]
pub fn open_in_siyuan(id: String) -> Result<String, String> {
    let id = id.trim().to_string();
    if !is_block_id(&id) {
        return Err(format!("不是合法的文档 ID：{}", id));
    }

    let url = format!("siyuan://blocks/{}", id);

    #[cfg(windows)]
    {
        // 经验：从脚本/服务上下文调用 `cmd /C start` 会被静默拦掉（不报错也不启动），
        // 交给 explorer.exe 走 ShellExecute 更稳，行为等同用户双击链接。
        let mut cmd = Command::new("explorer");
        cmd.arg(&url);
        cmd.creation_flags(CREATE_NO_WINDOW);
        match cmd.spawn() {
            Ok(_) => return Ok(url),
            Err(e) => {
                // 兜底再试一次传统方式
                let mut c2 = Command::new("cmd");
                c2.args(["/C", "start", "", &url]);
                c2.creation_flags(CREATE_NO_WINDOW);
                c2.spawn().map_err(|e2| format!("唤起失败：{} / {}", e, e2))?;
                return Ok(url);
            }
        }
    }

    #[cfg(not(windows))]
    {
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        Command::new(opener)
            .arg(&url)
            .spawn()
            .map_err(|e| format!("唤起失败：{}", e))?;
        Ok(url)
    }
}
