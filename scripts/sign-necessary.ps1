param(
    [Parameter(Mandatory, Position = 0)]
    [string]$File
)

$ErrorActionPreference = "Stop"

# --- Environment ---
$token = $env:NECESSARY_SIGN_TOKEN
$baseUrl = if ($env:NECESSARY_SIGN_URL) { $env:NECESSARY_SIGN_URL } else { "https://sign.necessary.nu" }

# --- Validate prerequisites ---
if (-not $token) {
    Write-Error "NECESSARY_SIGN_TOKEN environment variable is not set."
    exit 1
}

if (-not (Test-Path $File)) {
    Write-Error "File not found: $File"
    exit 1
}

# Find osslsigncode: check PATH, then known build locations
$osslsigncode = (Get-Command osslsigncode -ErrorAction SilentlyContinue).Source
if (-not $osslsigncode) {
    $osslsigncode = (Get-Command osslsigncode-rs -ErrorAction SilentlyContinue).Source
}
if (-not $osslsigncode) {
    $candidate = "$HOME\dev\osslsigncode\target\release\osslsigncode-rs.exe"
    if (Test-Path $candidate) { $osslsigncode = $candidate }
}
if (-not $osslsigncode) {
    Write-Error "osslsigncode not found on PATH or in ~/dev/osslsigncode/target/release/"
    exit 1
}

$resolved = (Resolve-Path $File).Path
$tempDir = Join-Path ([System.IO.Path]::GetTempPath()) "outto-sign-$([System.Guid]::NewGuid().ToString('N'))"

try {
    New-Item -ItemType Directory -Path $tempDir | Out-Null

    $tosign  = Join-Path $tempDir "tosign.bin"
    $signed  = Join-Path $tempDir "signed.bin"
    $output  = Join-Path $tempDir "signed.exe"

    # Step 1: Extract signing data from PE
    Write-Host "Extracting signing data from $resolved..." -ForegroundColor Cyan
    $ErrorActionPreference = "Continue"
    & osslsigncode extract-data --in $resolved --out $tosign 2>$null
    $ErrorActionPreference = "Stop"
    if ($LASTEXITCODE -ne 0) {
        Write-Error "osslsigncode extract-data failed (exit code $LASTEXITCODE)"
        exit 1
    }

    # Step 2: POST to sign.necessary.nu (retry once on failure)
    $signUrl = "$baseUrl/windows/sign"
    Write-Host "Sending to $signUrl..." -ForegroundColor Cyan

    $maxAttempts = 2
    for ($attempt = 1; $attempt -le $maxAttempts; $attempt++) {
        & curl.exe -sf -X POST `
            -H "Authorization: Bearer $token" `
            --data-binary "@$tosign" `
            $signUrl `
            -o $signed 2>&1

        if ($LASTEXITCODE -eq 0 -and (Test-Path $signed) -and (Get-Item $signed).Length -gt 0) {
            break
        }

        if ($attempt -lt $maxAttempts) {
            Write-Host "Attempt $attempt failed, retrying in 3s..." -ForegroundColor Yellow
            Start-Sleep -Seconds 3
        } else {
            Write-Error "Signing API request failed after $maxAttempts attempts (exit code $LASTEXITCODE)"
            exit 1
        }
    }

    # Step 3: Attach signature back to PE
    Write-Host "Attaching signature..." -ForegroundColor Cyan
    $ErrorActionPreference = "Continue"
    & osslsigncode attach-signature --sigin $signed --in $resolved --out $output 2>$null
    $ErrorActionPreference = "Stop"
    if ($LASTEXITCODE -ne 0) {
        Write-Error "osslsigncode attach-signature failed (exit code $LASTEXITCODE)"
        exit 1
    }

    # Step 4: Replace original with signed version
    Copy-Item -Path $output -Destination $resolved -Force
    Write-Host "Signed: $resolved" -ForegroundColor Green
}
finally {
    if (Test-Path $tempDir) {
        Remove-Item -Recurse -Force $tempDir -ErrorAction SilentlyContinue
    }
}
