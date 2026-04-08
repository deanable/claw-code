param(
    [string]$Prompt,
    [switch]$Help,
    [switch]$NoNewWindow
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$rustRoot = Join-Path $repoRoot "rust"
$cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
$cargoExe = Join-Path $cargoBin "cargo.exe"
$clawExe = Join-Path $rustRoot "target\debug\claw.exe"

if ($Help) {
    Write-Host "Usage:"
    Write-Host "  .\launch-claw-groq.ps1"
    Write-Host "  .\launch-claw-groq.ps1 -Prompt ""summarize this repository"""
    Write-Host "  .\launch-claw-groq.ps1 -NoNewWindow"
    Write-Host ""
    Write-Host "Environment:"
    Write-Host "  OPENAI_API_KEY   Required. Your Groq API key."
    Write-Host "  OPENAI_BASE_URL  Optional. Defaults to https://api.groq.com/openai/v1"
    Write-Host "  CLAW_MODEL       Optional. Defaults to llama-3.3-70b-versatile"
    exit 0
}

if (-not (Test-Path $cargoExe)) {
    throw "cargo.exe was not found at $cargoExe"
}

if (-not $NoNewWindow) {
    $argList = @(
        "-NoExit"
        "-ExecutionPolicy"
        "Bypass"
        "-File"
        ('"{0}"' -f $MyInvocation.MyCommand.Path)
        "-NoNewWindow"
    )

    if ($Prompt) {
        $argList += @("-Prompt", ('"{0}"' -f $Prompt.Replace('"', '\"')))
    }

    Start-Process powershell.exe -WorkingDirectory $repoRoot -ArgumentList $argList
    exit 0
}

if (-not (Test-Path $clawExe)) {
    Write-Host "Building claw first..."
    Push-Location $rustRoot
    try {
        & $cargoExe build --workspace
    }
    finally {
        Pop-Location
    }
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed"
    }
}

$env:PATH = "$cargoBin;$env:PATH"
$env:HOME = $env:USERPROFILE
$env:OPENAI_BASE_URL = if ($env:OPENAI_BASE_URL) { $env:OPENAI_BASE_URL } else { "https://api.groq.com/openai/v1" }
$model = if ($env:CLAW_MODEL) { $env:CLAW_MODEL } else { "llama-3.3-70b-versatile" }

Remove-Item Env:ANTHROPIC_API_KEY -ErrorAction SilentlyContinue

if (-not $env:OPENAI_API_KEY) {
    $secureKey = Read-Host "Enter your Groq API key" -AsSecureString
    $bstr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secureKey)
    try {
        $env:OPENAI_API_KEY = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($bstr)
    }
    finally {
        if ($bstr -ne [IntPtr]::Zero) {
            [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($bstr)
        }
    }
}

Push-Location $rustRoot
try {
    Write-Host "Launching Claw in $rustRoot"
    Write-Host "Model: $model"
    Write-Host "Base URL: $($env:OPENAI_BASE_URL)"
    if ($Prompt) {
        & $clawExe --model $model prompt $Prompt
    }
    else {
        & $clawExe --model $model
    }

    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
}
finally {
    Pop-Location
}
