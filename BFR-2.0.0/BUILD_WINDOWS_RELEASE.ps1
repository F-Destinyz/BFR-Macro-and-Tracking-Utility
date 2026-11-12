$ErrorActionPreference = 'Stop'

function Require-Command([string]$Name) {
    if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
        throw "Required command '$Name' was not found. Install Node.js/npm and the Rust MSVC toolchain, then try again."
    }
}

function Invoke-Npm([string[]]$Arguments, [string]$FailureMessage) {
    & npm @Arguments
    if ($LASTEXITCODE -ne 0) { throw $FailureMessage }
}

function Invoke-Cargo([string[]]$Arguments, [string]$FailureMessage) {
    & cargo @Arguments
    if ($LASTEXITCODE -ne 0) { throw $FailureMessage }
}

Require-Command 'node'
Require-Command 'npm'
Require-Command 'cargo'

Push-Location $PSScriptRoot
try {
    Write-Host 'Installing locked project dependencies...' -ForegroundColor Cyan
    Invoke-Npm @('ci') 'npm ci failed.'

    Write-Host 'Running automated release-contract checks...' -ForegroundColor Cyan
    Invoke-Npm @('test') 'Automated release checks failed; installer build cancelled.'

    Write-Host 'Running Rust unit tests...' -ForegroundColor Cyan
    Invoke-Cargo @('test', '--locked', '--manifest-path', 'src-tauri\Cargo.toml') 'Rust unit tests failed.'

    Write-Host 'Running Rust Clippy...' -ForegroundColor Cyan
    Invoke-Cargo @('clippy', '--locked', '--manifest-path', 'src-tauri\Cargo.toml', '--all-targets', '--', '-D', 'warnings') 'Rust Clippy reported warnings or errors.'

    Write-Host 'Building the BFR Macro & Tracking Utility 2.0.1 Windows installer...' -ForegroundColor Cyan
    Invoke-Npm @('run', 'build:windows') 'The Tauri Windows build failed. Review the compiler output above.'

    $installerDirectory = Join-Path $PSScriptRoot 'src-tauri\target\release\bundle\nsis'
    $installers = Get-ChildItem -Path $installerDirectory -Filter '*setup.exe' -ErrorAction SilentlyContinue
    if (-not $installers) {
        throw "Build completed but no NSIS setup executable was found in: $installerDirectory"
    }

    Write-Host ''
    Write-Host 'BFR Macro & Tracking Utility 2.0.1 installer(s):' -ForegroundColor Green
    $installers | ForEach-Object { Write-Host $_.FullName }
    Write-Host ''
    Write-Host 'The stable application identifier is retained so the existing per-user data remains available.' -ForegroundColor Yellow
}
finally {
    Pop-Location
}
