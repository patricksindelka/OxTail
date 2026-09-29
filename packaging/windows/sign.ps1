# Signs the given files with signtool using a PFX from the environment.
# Env: WINDOWS_CERT_PFX_BASE64 (base64 of the .pfx), WINDOWS_CERT_PASSWORD,
#      WINDOWS_TIMESTAMP_URL (optional, defaults to DigiCert).
# Usage: pwsh packaging/windows/sign.ps1 dist\stage\oxtail.exe dist\oxtail.msi
param([Parameter(ValueFromRemainingArguments = $true)][string[]]$Files)
$ErrorActionPreference = 'Stop'
if (-not $env:WINDOWS_CERT_PFX_BASE64) { throw 'WINDOWS_CERT_PFX_BASE64 is not set' }
$ts = if ($env:WINDOWS_TIMESTAMP_URL) { $env:WINDOWS_TIMESTAMP_URL } else { 'http://timestamp.digicert.com' }
$signtool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" |
    Sort-Object { [version]$_.Directory.Parent.Name } | Select-Object -Last 1
if (-not $signtool) { throw 'signtool.exe not found (Windows SDK missing)' }
$pfx = Join-Path $env:RUNNER_TEMP 'oxtail-codesign.pfx'
[IO.File]::WriteAllBytes($pfx, [Convert]::FromBase64String($env:WINDOWS_CERT_PFX_BASE64))
try {
    foreach ($f in $Files) {
        & $signtool.FullName sign /fd sha256 /f $pfx /p $env:WINDOWS_CERT_PASSWORD /tr $ts /td sha256 /d 'OxTail' $f
        if ($LASTEXITCODE -ne 0) { throw "signtool failed for $f" }
        & $signtool.FullName verify /pa $f
        if ($LASTEXITCODE -ne 0) { throw "signature verification failed for $f" }
    }
} finally {
    Remove-Item $pfx -Force -ErrorAction SilentlyContinue
}
