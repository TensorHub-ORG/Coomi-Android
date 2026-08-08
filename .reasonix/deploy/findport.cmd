@echo off
set PATH=%LOCALAPPDATA%\Android\Sdk\platform-tools;%PATH%
adb forward --remove-all >nul 2>&1
adb shell "logcat -d | grep -oE 'listening on http://127.0.0.1:[0-9]+' | tail -1" > "%~dp0port.txt" 2>&1
type "%~dp0port.txt"
