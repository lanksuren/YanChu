@echo off
REM YanChu build entry point.
REM
REM Why this exists: rustc only locates link.exe when it can find a VS install
REM itself. On this machine vswhere is not on PATH, so a bare `cargo build`
REM fails with "linker link.exe not found". Entering the MSVC environment
REM first fixes it.
REM
REM Keep this file ASCII-only: cmd.exe reads .bat as ANSI, and non-ASCII
REM comments get mangled into stray commands.
REM
REM Usage:
REM   tools\build.bat                  -> cargo build
REM   tools\build.bat run              -> cargo run
REM   tools\build.bat build --features asr

setlocal
set "VCVARS=C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"

if not exist "%VCVARS%" (
  echo [YanChu] vcvars64.bat not found. Install VS Build Tools with the "Desktop development with C++" workload.
  exit /b 1
)

call "%VCVARS%" >nul
if errorlevel 1 (
  echo [YanChu] vcvars64 failed to initialize.
  exit /b 1
)

set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
cd /d "%~dp0..\app\src-tauri"

if "%~1"=="" (
  cargo build
) else (
  cargo %*
)

endlocal
