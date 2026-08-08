@echo off
set PATH=%LOCALAPPDATA%\Android\Sdk\platform-tools;%PATH%
adb shell "run-as com.coomi.android sh -c 'pid=$(ps -A | grep libcoomi | head -1 | awk \"{print \\$2}\"); echo PID=$pid; cat /proc/$pid/cmdline | tr \"\\0\" \" \"; echo'" > "%~dp0cmdline.txt" 2>&1
type "%~dp0cmdline.txt"
