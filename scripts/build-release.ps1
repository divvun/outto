param([string]$Sign)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$triple = "x86_64-pc-windows-msvc"
$target = "$root\target\$triple\release"

# Build all crates with nightly, rebuilt std, and panic=immediate-abort
Write-Host "Building workspace in release mode (nightly + build-std)..." -ForegroundColor Cyan
Push-Location $root
cargo +nightly build --release --workspace `
    -Zbuild-std=std `
    --target $triple
if ($LASTEXITCODE -ne 0) { Pop-Location; exit $LASTEXITCODE }
Pop-Location

# Stage into bin/ + libexec/ layout
$stage = "$root\target\installer-stage"
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory -Path "$stage\bin" | Out-Null
New-Item -ItemType Directory -Path "$stage\libexec" | Out-Null

Copy-Item "$target\outto.exe" "$stage\bin\outto.exe"
Copy-Item "$target\outto-gui.exe" "$stage\libexec\"
Copy-Item "$target\outto-sfx.exe" "$stage\libexec\"
Copy-Item "$target\outto-uninstall.exe" "$stage\libexec\"

Write-Host "Staged layout:" -ForegroundColor Cyan
Get-ChildItem $stage -Recurse -File | ForEach-Object {
    $rel = $_.FullName.Substring($stage.Length + 1)
    Write-Host "  $rel ($([math]::Round($_.Length / 1MB, 1)) MB)"
}

# Package with outto
$cli = "$target\outto.exe"
$output = "$root\target\release\outto-setup.exe"
$buildArgs = @("build", "--config", "$root\outto.toml", "--source", $stage, "--output", $output, "--compress", "--compression-level", "22")
if ($Sign) { $buildArgs += @("--sign", $Sign) }

Write-Host "Packaging outto installer..." -ForegroundColor Cyan
& $cli @buildArgs
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$size = [math]::Round((Get-Item $output).Length / 1MB, 1)
Write-Host "Done: $output ($size MB)" -ForegroundColor Green
