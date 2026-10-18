@echo off
setlocal EnableExtensions

REM ===========================================================================
REM  sainput build entry point.
REM
REM  Usage:
REM    build.bat                    same as "build.bat build --release"
REM    build.bat build --release    explicit release build
REM    build.bat run                cargo run
REM    build.bat test               cargo test
REM    build.bat <anything>         passed straight to cargo
REM
REM  Toolchain detection (only needed for the x86_64-pc-windows-gnu target):
REM    1. %W64DEVKIT%\bin\gcc.exe
REM    2. tools\w64devkit\bin\gcc.exe inside the repo
REM    3. gcc.exe already on PATH
REM    4. common install dirs: C:\msys64\mingw64, %ProgramFiles%\w64devkit,
REM       C:\tools\w64devkit
REM  If .cargo\config.toml already pins "linker = ...", gcc does not have to be
REM  on PATH at all - cargo calls that compiler directly.
REM
REM  NOTE: keep this file pure ASCII. cmd.exe reads .bat files with the OEM
REM  code page; non-ASCII comments turn into mojibake and get executed.
REM ===========================================================================

cd /d "%~dp0"

REM ---- does the current configuration ask for the GNU target? ----
set "NEED_GNU=0"
if exist ".cargo\config.toml" findstr /i /c:"x86_64-pc-windows-gnu" ".cargo\config.toml" >nul 2>&1 && set "NEED_GNU=1"
if exist "rust-toolchain.toml" findstr /i /c:"windows-gnu" "rust-toolchain.toml" >nul 2>&1 && set "NEED_GNU=1"

REM ---- an uncommented "linker = ..." makes a PATH lookup unnecessary ----
if "%NEED_GNU%"=="1" if exist ".cargo\config.toml" (
    findstr /r /c:"^[ 	]*linker" ".cargo\config.toml" >nul 2>&1 && set "NEED_GNU=0"
)

REM ---- locate a GNU linker ----
set "GCC_DIR="
if defined W64DEVKIT if exist "%W64DEVKIT%\bin\gcc.exe" set "GCC_DIR=%W64DEVKIT%\bin"
if not defined GCC_DIR if exist "%~dp0tools\w64devkit\bin\gcc.exe" set "GCC_DIR=%~dp0tools\w64devkit\bin"
if not defined GCC_DIR for %%I in (gcc.exe) do if not "%%~$PATH:I"=="" set "GCC_DIR=%%~dp$PATH:I"
if not defined GCC_DIR if exist "C:\msys64\mingw64\bin\gcc.exe" set "GCC_DIR=C:\msys64\mingw64\bin"
if not defined GCC_DIR if exist "%ProgramFiles%\w64devkit\bin\gcc.exe" set "GCC_DIR=%ProgramFiles%\w64devkit\bin"
if not defined GCC_DIR if exist "C:\tools\w64devkit\bin\gcc.exe" set "GCC_DIR=C:\tools\w64devkit\bin"

REM normalize to a full path without a trailing backslash:
REM "%%~fD" removes the trailing separator, which keeps PATH well formed.
if defined GCC_DIR for %%D in ("%GCC_DIR%") do set "GCC_DIR=%%~fD"

if "%NEED_GNU%"=="1" (
    if not defined GCC_DIR (
        echo.
        echo   [sainput] This repo is configured for x86_64-pc-windows-gnu,
        echo   [sainput] but no gcc.exe was found.
        echo.
        echo   Fix it in one of these ways:
        echo     - install w64devkit and set   set W64DEVKIT=C:\path\to\w64devkit
        echo     - or put mingw-w64 gcc on PATH ^(e.g. C:\msys64\mingw64\bin^)
        echo     - or point .cargo\config.toml at your compiler
        echo       ^(see .cargo\config.toml.example, section B^)
        echo     - or switch back to MSVC: rename rust-toolchain.toml and
        echo       .cargo\config.toml, then install VS Build Tools
        echo.
        exit /b 1
    )
)

if defined GCC_DIR (
    set "PATH=%GCC_DIR%;%PATH%"
    echo [sainput] gcc: %GCC_DIR%\gcc.exe
)

where cargo >nul 2>&1
if errorlevel 1 (
    echo.
    echo   [sainput] cargo was not found on PATH.
    echo   [sainput] Install Rust from https://rustup.rs and reopen the terminal.
    echo.
    exit /b 1
)

if "%~1"=="" (
    echo [sainput] cargo build --release
    call cargo build --release
) else (
    echo [sainput] cargo %*
    call cargo %*
)

set "RC=%ERRORLEVEL%"
if not "%RC%"=="0" (
    echo.
    echo   [sainput] cargo exited with code %RC%
)
endlocal & exit /b %RC%
