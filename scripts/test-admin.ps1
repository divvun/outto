# Run admin integration tests in an elevated shell.
# Usage: powershell -ExecutionPolicy Bypass -File scripts/test-admin.ps1

$projectRoot = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)

Start-Process -Verb RunAs -Wait -FilePath "cargo" `
    -ArgumentList "nextest run -P admin --run-ignored ignored-only" `
    -WorkingDirectory $projectRoot
