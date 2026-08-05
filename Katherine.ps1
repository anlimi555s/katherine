$khome = $PSScriptRoot
$env:KATHERINE_HOME = $khome

# 从 .env 加载密钥（.env 在 .gitignore 中，不会提交）
$envFile = Join-Path $khome ".env"
if (Test-Path $envFile) {
    Get-Content $envFile | ForEach-Object {
        if ($_ -match '^\s*([^#].+?)\s*=\s*(.+)$') {
            $name = $matches[1].Trim()
            $value = $matches[2].Trim()
            if ($name -notmatch '^#') {
                Set-Item -Path "env:$name" -Value $value
            }
        }
    }
} else {
    Write-Host "[Katherine] .env not found. Set ANTHROPIC_AUTH_TOKEN before starting."
}

# 默认值（.env 已设置则不会覆盖）
if (-not $env:ANTHROPIC_BASE_URL) { $env:ANTHROPIC_BASE_URL = "https://api.deepseek.com/anthropic" }
if (-not $env:ANTHROPIC_MODEL) { $env:ANTHROPIC_MODEL = "deepseek-v4-pro[1m]" }

Write-Host "Katherine — starting engine..."
Start-Process -FilePath "$khome\target\release\katherine-cli.exe" -ArgumentList "serve","--port","9876" -WindowStyle Normal
Start-Sleep 4

# 找 Chrome：和引擎 browser.rs find_chrome() 同样逻辑
$chrome = $null
if ($env:CHROME_PATH -and (Test-Path $env:CHROME_PATH)) {
    $chrome = $env:CHROME_PATH
} elseif (Test-Path "$env:KATHERINE_HOME\bin\chrome-win\chrome.exe") {
    $chrome = "$env:KATHERINE_HOME\bin\chrome-win\chrome.exe"
} else {
    foreach ($p in @("C:\Program Files\Google\Chrome\Application\chrome.exe",
                     "C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
                     "$env:LOCALAPPDATA\Google\Chrome\Application\chrome.exe")) {
        if (Test-Path $p) { $chrome = $p; break }
    }
}

$html = "$khome\katherine-memories\katherine.html"
if ($chrome) {
    Write-Host "Chrome: $chrome"
    Start-Process -FilePath $chrome -ArgumentList "--app=$html","--window-size=520,750"
} else {
    Write-Host "Chrome not found, opening in default browser"
    Start-Process $html
}
Write-Host "Ready."
