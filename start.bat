@echo off
setlocal EnableExtensions

REM ===========================================================================
REM  sainput launcher - double-click this file to start the app.
REM
REM  Why it is written this way (a trap we already hit - do not "simplify" it):
REM    sainput.exe is a console-subsystem program, so Windows always gives it a
REM    console. On Windows 11 that console is hosted by WindowsTerminal.exe and
REM    does NOT belong to the sainput process - so GetConsoleWindow() +
REM    ShowWindow(SW_HIDE) inside sainput cannot hide it.
REM    Correct approach: let sainput inherit THIS script's console ("start /b"
REM    does not create a new window). sainput calls FreeConsole() and detaches
REM    itself right after startup; this script then exits. The console belongs
REM    to the script, so it disappears together with the script, while sainput
REM    - already detached - keeps running.
REM
REM  Keep this file pure ASCII. cmd.exe reads .bat files with the OEM code
REM  page; non-ASCII comments turn into mojibake and get executed.
REM ===========================================================================

cd /d "%~dp0"

set "PYTHONIOENCODING=utf-8"

REM ---- find the binary: MSVC layout first, then GNU layout ----
set "EXE="
if exist "target\release\sainput.exe" set "EXE=target\release\sainput.exe"
if not defined EXE if exist "target\x86_64-pc-windows-gnu\release\sainput.exe" set "EXE=target\x86_64-pc-windows-gnu\release\sainput.exe"

if not defined EXE (
    echo.
    echo   [sainput] sainput.exe not found.
    echo   [sainput] Looked in:
    echo   [sainput]   target\release\sainput.exe
    echo   [sainput]   target\x86_64-pc-windows-gnu\release\sainput.exe
    echo.
    echo   [sainput] Build it first:   build.bat
    echo.
    pause
    exit /b 1
)

echo.
echo   sainput - local voice input - starting, please wait...
echo   No window will stay open; the tray icon means it is running.
echo   Quit: right-click the tray icon, then Exit.
echo.

REM "start /b" = do not create a new window (inherit this console).
REM The console stays usable for println/log output until sainput detaches.
start "" /b "%EXE%" run --hide-console

endlocal
exit /b 0
