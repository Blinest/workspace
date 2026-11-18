# 版本号管理

## 规则

| 改动量级 | 版本变化 | 谁来定 |
|---|---|---|
| **小改动**（修 bug、加小功能、调样式） | 补丁位 +1：`0.0.2` → `0.0.3` | 自动，每次改完就升 |
| **中等更新**（几项功能合并、结构调整） | 次版本位 +1：`0.0.x` → `0.1.0` | 由我提示 |
| **大版本**（架构重写、正式发布） | 主版本位 +1：`0.x.x` → `1.0.0` | 由我提示 |

## 一次小版本升级要改的地方

版本号分散在 6 个地方，**漏掉任何一处都会导致安装包内版本不一致**：

```
src-tauri/tauri.conf.json     "version": "0.0.2"     ← 安装包文件名用这个
src-tauri/Cargo.toml          version = "0.0.2"      ← 编译进 exe 的资源
package.json                  "version": "0.0.2"
package-lock.json             两处：顶层 + packages[""]
frontend/index.html           文件头注释 + 页脚「v0.0.2」
```

### 推荐做法

```bash
# 1. 改完代码后，一次性替换（Windows PowerShell）
#    注意：不要用 ConvertFrom-Json / ConvertTo-Json，会破坏中文
```

逐文件用编辑器改，或者用下面的命令（**仅限纯 ASCII 的 JSON/TOML 文件**）：

```powershell
$old = "0.0.2"; $new = "0.0.3"
foreach ($f in @("src-tauri\tauri.conf.json","src-tauri\Cargo.toml","package.json","package-lock.json")) {
    $c = Get-Content $f -Raw -Encoding UTF8
    $c = $c.Replace('"version": "' + $old + '"', '"version": "' + $new + '"')
    $c = $c.Replace('version = "' + $old + '"', 'version = "' + $new + '"')
    [System.IO.File]::WriteAllText((Resolve-Path $f), $c, (New-Object System.Text.UTF8Encoding($false)))
}
```

`frontend/index.html` 含大量中文，**必须用编辑器改**，不要用上面的脚本。

### 改完自检

```powershell
# 应该输出「没有残留」
node -e "..."   # 见 scripts/check-version.js
```

或手工确认：

```powershell
Select-String -Path src-tauri\tauri.conf.json -Pattern '"version"'
Select-String -Path src-tauri\Cargo.toml -Pattern '^version'
Select-String -Path package.json -Pattern '"version"'
Select-String -Path frontend\index.html -Pattern 'v0\.0\.'
```

## 产物命名

安装包由 Tauri 按 `tauri.conf.json` 的 `version` 自动命名：

```
SiyuanResearchWorktable_0.0.2_x64-setup.exe
```

## 提交信息约定

```
feat: 简述           新功能
fix: 简述            修 bug
chore: 升版本到 0.0.3  纯版本号变更
```
