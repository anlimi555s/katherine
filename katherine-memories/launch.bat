@echo off
REM Katherine Desktop Shell — Chrome app mode 独立窗口
REM 脱离 VSCode，chat + dashboard 分开两个窗口

set "CHAT=%~dp0chat.html"
set "DASHBOARD=%~dp0dashboard.html"

REM 找 Chrome
set "CHROME="
if exist "C:\Program Files\Google\Chrome\Application\chrome.exe" set "CHROME=C:\Program Files\Google\Chrome\Application\chrome.exe"
if exist "C:\Program Files (x86)\Google\Chrome\Application\chrome.exe" set "CHROME=C:\Program Files (x86)\Google\Chrome\Application\chrome.exe"
if exist "%LOCALAPPDATA%\Google\Chrome\Application\chrome.exe" set "CHROME=%LOCALAPPDATA%\Google\Chrome\Application\chrome.exe"

if "%CHROME%"=="" (
    echo Chrome not found. Open %CHAT% in your browser.
    start "" "%CHAT%"
    start "" "%DASHBOARD%"
    exit /b
)

echo Starting Katherine Desktop...
REM --app mode: 独立窗口，无浏览器外壳
start "Katherine" "%CHROME%" --app="%KATHERINE_FILE%" --window-size=520,750 --window-position=50,50

echo Chat window + Dashboard window ready.
