@echo off
REM Start YanChu with development capture enabled.
REM
REM Every "hold to talk" writes its audio and the full pipeline record to
REM ..\devlog  (session.json / audio.wav / seg-NN.wav / events.jsonl / app.log).
REM Off by default in the product build: audio must not be written to disk.
REM
REM Usage:
REM   tools\dev-run.bat                    start with capture on
REM   set YANCHU_DEV_LOG_DIR=D:\tmp\log    capture somewhere else
REM   tools\dev-run.bat --polish-test "..."  extra args are passed through
REM
REM Keep this file ASCII-only (cmd.exe reads .bat as ANSI).
REM Note: launch via "cd" + bare exe name. A quoted path containing ".."
REM made cmd's start pop up "Windows cannot find ..." on this machine.

setlocal
set "BIN=%~dp0..\app\src-tauri\target\debug"
set "EXE=%BIN%\yanchu.exe"

if not exist "%EXE%" (
  echo [YanChu] not built yet: %EXE%
  echo [YanChu] build first: tools\build.bat build --features asr
  exit /b 1
)

set "YANCHU_DEV_LOG=1"
if not defined YANCHU_DEV_LOG_DIR set "YANCHU_DEV_LOG_DIR=%~dp0..\devlog"

echo [YanChu] capture on: %YANCHU_DEV_LOG_DIR%
cd /d "%BIN%"
start "" yanchu.exe %*
endlocal
