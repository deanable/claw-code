param(
    [string]$Workspace = "C:\Users\Dean\source\repos\Premiere Project builder"
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$rustRoot = Join-Path $repoRoot "rust"
$releaseDir = Join-Path $repoRoot "release"
$cargoExe = Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe"

if (-not (Test-Path $cargoExe)) {
    throw "cargo.exe was not found at $cargoExe"
}

Push-Location $rustRoot
try {
    & $cargoExe build --release -p rusty-claude-cli --bin claw --bin claw-launcher
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build --release failed"
    }
}
finally {
    Pop-Location
}

New-Item -ItemType Directory -Force -Path $releaseDir | Out-Null
Copy-Item -Force (Join-Path $rustRoot "target\release\claw.exe") (Join-Path $releaseDir "claw.exe")
Copy-Item -Force (Join-Path $rustRoot "target\release\claw-launcher.exe") (Join-Path $releaseDir "claw-launcher.exe")

$configPath = Join-Path $releaseDir "claw-launcher.json"
$config = @"
{
  "workspace": "$($Workspace -replace '\\','\\')",
  "model": "llama-3.3-70b-versatile",
  "permissionMode": "danger-full-access",
  "allowedTools": ["read", "glob", "grep"],
  "openaiApiKey": "",
  "openaiBaseUrl": "https://api.groq.com/openai/v1",
  "args": []
}
"@
Set-Content -Path $configPath -Value $config -Encoding UTF8

$readmePath = Join-Path $releaseDir "README.txt"
$readme = @"
Double-click claw-launcher.exe to start the terminal app.

Files in this folder:
- claw.exe: the Rust CLI
- claw-launcher.exe: the Windows launcher that reads claw-launcher.json
- claw-launcher.json: your launch config sidecar

Before first run:
1. Put your API key into claw-launcher.json.
2. Update the workspace path if needed.

If the VC++ runtime is missing, claw-launcher.exe opens the Microsoft installer page automatically.
"@
Set-Content -Path $readmePath -Value $readme -Encoding UTF8

Write-Host "Release package created in $releaseDir"
Write-Host "Edit $configPath and then double-click claw-launcher.exe"
