# Signs the given files with signtool using a PFX from the environment.
# Env: WINDOWS_CERT_PFX_BASE64 (base64 of the .pfx), WINDOWS_CERT_PASSWORD,
#      WINDOWS_TIMESTAMP_URL (optional, defaults to DigiCert),
#      WINDOWS_CERT_ROOT_BASE64 (optional: the root certificate of a private CA
#      such as step-ca, as PEM text or base64 of a PEM or DER file; trusted on
#      this machine while signing so the signature can be verified).
# Usage: pwsh packaging/windows/sign.ps1 dist\stage\oxtail.exe dist\oxtail.msi
param([Parameter(ValueFromRemainingArguments = $true)][string[]]$Files)
$ErrorActionPreference = 'Stop'

# Decodes a base64 secret, ignoring line breaks and spaces (wrapped `base64`
# output); the error names the secret but never shows its value. The functions
# return `,$bytes` so PowerShell does not unroll the byte[] into object[].
function ConvertFrom-SecretBase64([string]$Name) {
    $v = [Environment]::GetEnvironmentVariable($Name) -replace '\s', ''
    try { return , [Convert]::FromBase64String($v) }
    catch { throw "$Name is not valid base64 ($($v.Length) characters); set it to the output of: base64 -w0 <file>" }
}

# The root certificate as DER bytes: from PEM text pasted as is, or from base64
# of a PEM or DER file.
function Get-RootCertBytes([string]$Name) {
    $pem = '-----BEGIN CERTIFICATE-----([^-]+)-----END CERTIFICATE-----'
    $raw = [Environment]::GetEnvironmentVariable($Name)
    if ($raw -notmatch $pem) {
        $raw = [Text.Encoding]::ASCII.GetString((ConvertFrom-SecretBase64 $Name))
        if ($raw -notmatch $pem) { return , (ConvertFrom-SecretBase64 $Name) }
    }
    try { return , [Convert]::FromBase64String(($Matches[1] -replace '\s', '')) }
    catch { throw "$Name holds a malformed PEM certificate" }
}

if (-not $env:WINDOWS_CERT_PFX_BASE64) { throw 'WINDOWS_CERT_PFX_BASE64 is not set' }
$ts = if ($env:WINDOWS_TIMESTAMP_URL) { $env:WINDOWS_TIMESTAMP_URL } else { 'http://timestamp.digicert.com' }
$signtool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" |
    Sort-Object { [version]$_.Directory.Parent.Name } | Select-Object -Last 1
if (-not $signtool) { throw 'signtool.exe not found (Windows SDK missing)' }
$pfx = Join-Path $env:RUNNER_TEMP 'oxtail-codesign.pfx'
[IO.File]::WriteAllBytes($pfx, (ConvertFrom-SecretBase64 'WINDOWS_CERT_PFX_BASE64'))
# A private CA's root is not trusted by Windows, so `verify /pa` below would
# fail. Trust it for the duration (LocalMachine: adding to CurrentUser\Root
# opens a confirmation dialog, which would hang a headless runner).
$rootStore = $null
$addedRoot = $null
try {
    if ($env:WINDOWS_CERT_ROOT_BASE64) {
        $root = [Security.Cryptography.X509Certificates.X509Certificate2]::new((Get-RootCertBytes 'WINDOWS_CERT_ROOT_BASE64'))
        $rootStore = [Security.Cryptography.X509Certificates.X509Store]::new('Root', 'LocalMachine')
        $rootStore.Open('ReadWrite')
        if ($rootStore.Certificates.Find('FindByThumbprint', $root.Thumbprint, $false).Count -eq 0) {
            $rootStore.Add($root)
            $addedRoot = $root
        }
        "Trusting root for verification: $($root.Subject) ($($root.Thumbprint))"
    }
    # signtool only says it cannot load the .pfx. .NET opens it the same way
    # (CryptoAPI on Windows) and says why: a wrong password or a format it
    # cannot read. A trailing line break (`echo pw | gh secret set`) is dropped.
    $pw = $env:WINDOWS_CERT_PASSWORD -replace '[\r\n]+$', ''
    $certs = [Security.Cryptography.X509Certificates.X509Certificate2Collection]::new()
    try { $certs.Import($pfx, $pw, 'EphemeralKeySet') }
    catch { throw "cannot open the .pfx (WINDOWS_CERT_PFX_BASE64 with WINDOWS_CERT_PASSWORD): $($_.Exception.GetBaseException().Message)" }
    $signer = $certs | Where-Object HasPrivateKey | Select-Object -First 1
    if (-not $signer) { throw 'the .pfx holds no certificate with its private key' }
    $eku = ($signer.Extensions | Where-Object { $_ -is [Security.Cryptography.X509Certificates.X509EnhancedKeyUsageExtension] }).EnhancedKeyUsages |
        ForEach-Object { $_.FriendlyName }
    "Signing certificate: $($signer.Subject); key $($signer.PublicKey.Oid.FriendlyName); usage: $($eku -join ', '); valid until $($signer.NotAfter.ToString('yyyy-MM-dd')); $($certs.Count) certificate(s) in the .pfx"
    foreach ($f in $Files) {
        & $signtool.FullName sign /fd sha256 /f $pfx /p $pw /tr $ts /td sha256 /d 'OxTail' $f
        if ($LASTEXITCODE -ne 0) { throw "signtool failed for $f" }
        & $signtool.FullName verify /pa $f
        if ($LASTEXITCODE -ne 0) { throw "signature verification failed for $f" }
    }
} finally {
    Remove-Item $pfx -Force -ErrorAction SilentlyContinue
    if ($addedRoot) { $rootStore.Remove($addedRoot) }
    if ($rootStore) { $rootStore.Close() }
}
