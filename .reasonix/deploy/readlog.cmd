@echo off
set PATH=%LOCALAPPDATA%\Android\Sdk\platform-tools;%PATH%
adb shell "cat /data/user/0/com.coomi.android/files/home/coomi.log" > "%~dp0coomi.log.txt" 2>&1
adb shell "logcat -d > %TEMP%\coomi_logcat.txt" 2>nul
type "%~dp0coomi.log.txt"
