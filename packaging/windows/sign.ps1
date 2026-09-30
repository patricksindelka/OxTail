# Signs the given files with signtool using a PFX from the environment.
# Env: WINDOWS_CERT_PFX_BASE64 (base64 of the .pfx), WINDOWS_CERT_PASSWORD,
#      WINDOWS_TIMESTAMP_URL (optional, defaults to DigiCert),
#      WINDOWS_CERT_ROOT_BASE64 (optional: base64 of the root certificate, PEM
#      or DER, of a private CA such as step-ca; trusted on this machine while
#      signing so the signature can be verified).
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
# A private CA's root is not trusted by Windows, so `verify /pa` below would
# fail. Trust it for the duration (LocalMachine: adding to CurrentUser\Root
# opens a confirmation dialog, which would hang a headless runner).
$rootStore = $null
$addedRoot = $null
try {
    if ($env:WINDOWS_CERT_ROOT_BASE64) {
        $bytes = [Convert]::FromBase64String($env:WINDOWS_CERT_ROOT_BASE64)
        $text = [Text.Encoding]::ASCII.GetString($bytes)
        if ($text -match '-----BEGIN CERTIFICATE-----([^-]+)-----END CERTIFICATE-----') {
            $bytes = [Convert]::FromBase64String(($Matches[1] -replace '\s', ''))
        }
        $root = [Security.Cryptography.X509Certificates.X509Certificate2]::new($bytes)
        $rootStore = [Security.Cryptography.X509Certificates.X509Store]::new('Root', 'LocalMachine')
        $rootStore.Open('ReadWrite')
        if ($rootStore.Certificates.Find('FindByThumbprint', $root.Thumbprint, $false).Count -eq 0) {
            $rootStore.Add($root)
            $addedRoot = $root
        }
        "Trusting root for verification: $($root.Subject) ($($root.Thumbprint))"
    }
    foreach ($f in $Files) {
        & $signtool.FullName sign /fd sha256 /f $pfx /p $env:WINDOWS_CERT_PASSWORD /tr $ts /td sha256 /d 'OxTail' $f
        if ($LASTEXITCODE -ne 0) { throw "signtool failed for $f" }
        & $signtool.FullName verify /pa $f
        if ($LASTEXITCODE -ne 0) { throw "signature verification failed for $f" }
    }
} finally {
    Remove-Item $pfx -Force -ErrorAction SilentlyContinue
    if ($addedRoot) { $rootStore.Remove($addedRoot) }
    if ($rootStore) { $rootStore.Close() }
}
