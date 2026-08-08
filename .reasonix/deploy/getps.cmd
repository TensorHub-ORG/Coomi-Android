@echo off
set PATH=%LOCALAPPDATA%\Android\Sdk\platform-tools;%PATH%
adb shell "run-as com.coomi.android sh -c 'ps -A | grep libcoomi'" > "%~dp0ps.txt" 2>&1
type "%~dp0ps.txt"
