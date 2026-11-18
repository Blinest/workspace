@echo off
REM ===========================================================================
REM  一键启动工作台的全部后台服务（Windows）
REM
REM   双击本文件，或在命令行运行：start-all.bat
REM   每个服务开一个独立窗口，关掉窗口就是停止对应服务。
REM
REM  已经在监听的端口会被跳过 —— 和工作台自己拉起的服务可以共存。
REM ===========================================================================
setlocal enabledelayedexpansion
cd /d "%~dp0"

if "%SIYUAN_API_URL%"=="" set "SIYUAN_API_URL=http://127.0.0.1:6806"

echo 工作台后台服务
echo   思源内核：%SIYUAN_API_URL%
echo   脚本目录：%CD%
echo.

where node >nul 2>&1
if errorlevel 1 (
    echo [错误] 找不到 node，请先安装 Node.js 20+
    pause
    exit /b 1
)
where python >nul 2>&1
if errorlevel 1 (
    echo [错误] 找不到 python，请先安装 Python 3.9+
    pause
    exit /b 1
)

call :maybe_start regen-server    3918 node regen-server.js
call :maybe_start zhihu-bridge    3921 python zhihu_bridge.py
call :maybe_start invoice-parser  3923 python invoice_parser.py
call :maybe_start scrapling-fetch 3920 python scrapling_fetch.py

echo.
echo 全部已启动（端口已占用的会跳过）。
echo 每个服务一个窗口，关闭窗口即停止该服务。
echo.
pause
exit /b 0

:maybe_start
REM %1=名称 %2=端口 其余=启动命令
set "NAME=%~1"
set "PORT=%~2"
shift & shift
netstat -ano | findstr /R /C:":%PORT% .*LISTENING" >nul 2>&1
if not errorlevel 1 (
    echo   [跳过] %NAME% —— 端口 %PORT% 已在监听
    exit /b 0
)
echo   [启动] %NAME%  ^(端口 %PORT%^)
start "%NAME%" cmd /k %*
exit /b 0
