@echo off
set KATHERINE_HOME=%~dp0
set ANTHROPIC_BASE_URL=https://api.deepseek.com/anthropic
set ANTHROPIC_MODEL=deepseek-v4-pro[1m]

echo Katherine — starting engine...
start "Katherine" "%KATHERINE_HOME%target\release\katherine-cli.exe" serve --port 9876
timeout /t 4 /nobreak >nul

echo Opening browser...
start "" "%KATHERINE_HOME%katherine-memories\katherine.html"
echo Ready.
