@echo off
set PATH=%LOCALAPPDATA%\Android\Sdk\platform-tools;%PATH%
adb forward tcp:46085 tcp:46085 >nul 2>&1
curl -s -o NUL -w "NO-TOKEN:%%{http_code}\n" http://127.0.0.1:46085/api/runtime/health
curl -s -H "Authorization: Bearer cf6affc70f491d01f93c7a4ff7efe0d27a6a33b7ccbfe5ee61e0af480f18321f00c4bae0cbfdfb7de83a20ca1b439080ca3d9775e1455b678761e5a3dbc61e53" -o "%~dp0health.txt" -w "WITH-TOKEN:%%{http_code}\n" http://127.0.0.1:46085/api/runtime/health
type "%~dp0health.txt"
