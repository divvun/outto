$triple = "x86_64-pc-windows-msvc"
$cli = ".\target\$triple\release\outto.exe"

cargo +nightly build --release -p outto-gui -p outto-uninstall -p outto-sfx -p outto-cli `
    -Zbuild-std=std `
    --target $triple
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Remove-Item -Force test-installer-compressed.exe -ErrorAction SilentlyContinue

& $cli build --config test-source\outto.toml --source test-source --output test-installer-compressed.exe --compress
