$khome = "C:\Users\Selena\Desktop\Katherine\katherine-rust"
$env:KATHERINE_HOME = $khome
$env:ANTHROPIC_BASE_URL = "https://api.deepseek.com/anthropic"
$env:ANTHROPIC_AUTH_TOKEN = "sk-af98d5f7ba6743b693b5fbd8508b8c86"
$env:ANTHROPIC_MODEL = "deepseek-v4-pro[1m]"

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
