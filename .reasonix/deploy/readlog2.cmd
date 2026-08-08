@echo off
set PATH=%LOCALAPPDATA%\Android\Sdk\platform-tools;%PATH%
adb shell "run-as com.coomi.android cat files/home/coomi.log" > "%~dp0coomi.log.txt" 2>&1
type "%~dp0coomi.log.txt"
