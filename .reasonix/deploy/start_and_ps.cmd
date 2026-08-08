@echo off
set PATH=%LOCALAPPDATA%\Android\Sdk\platform-tools;%PATH%
adb shell "am start -n com.coomi.android/app.coomi.CoomiLauncherActivity" >nul 2>&1
ping -n 13 127.0.0.1 >nul
adb shell "run-as com.coomi.android sh -c 'ps -A | grep libcoomi'" > "%~dp0ps.txt" 2>&1
type "%~dp0ps.txt"
