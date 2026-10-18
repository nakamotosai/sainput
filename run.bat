@echo off
setlocal EnableExtensions

REM ===========================================================================
REM  Run sainput in the foreground and pass arguments through.
REM
REM    run.bat                 print CLI help
REM    run.bat devices         list microphones
REM    run.bat run             stay resident and type what you say
REM    run.bat replay in.wav --dry-run
REM
REM  For normal use, double-click start.bat instead: it detaches the console
REM  so no window stays on screen. This file keeps the window on purpose, so
REM  you can read the output while debugging.
REM
REM  Keep this file pure ASCII (see build.bat header for the reason).
REM ===========================================================================

cd /d "%~dp0"

set "PYTHONIOENCODING=utf-8"

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

"%EXE%" %*
exit /b %ERRORLEVEL%
