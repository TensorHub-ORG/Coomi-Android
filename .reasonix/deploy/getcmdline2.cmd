@echo off
set PATH=%LOCALAPPDATA%\Android\Sdk\platform-tools;%PATH%
adb shell "run-as com.coomi.android sh -c 'for p in 15222 15226 15312; do echo PID=$p; cat /proc/$p/cmdline | tr \"\\0\" \" \"; echo; done'" > "%~dp0cmdline.txt" 2>&1
type "%~dp0cmdline.txt"
