<#
Eggpass PowerShell bootstrap installer, adapted from eggstack/gregg packaging/install.ps1
at fabcaf21bc60f719a5de5b9420490ffed52400e4.
Copyright (c) 2026 The Gregg Authors. MIT License.
Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

.SYNOPSIS
    Install Eggpass from a verified GitHub release.
.DESCRIPTION
    The default is a per-user install under LOCALAPPDATA. Use -System explicitly
    from an elevated PowerShell session for a system-wide install. No sudo or
    elevation prompt is launched. Only the five Eggpack release targets exist.
.PARAMETER Version
    Pin the exact stable GitHub CLI release X.Y.Z (optional leading v accepted).
.PARAMETER System
    Install under ProgramFiles; requires an already elevated session.
.EXAMPLE
    irm https://github.com/eggstack/eggpass/releases/latest/download/install.ps1 | iex
.EXAMPLE
    .\install.ps1 -Version 1.2.3
#>
[CmdletBinding()]
param(
    [string]$Version,
    [switch]$System
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$Repo = "eggstack/eggpass"
$BaseUrl = "https://github.com/$Repo/releases"
$StrippedVersion = ""
$Tag = ""
if ($PSBoundParameters.ContainsKey("Version")) {
    if ([string]::IsNullOrWhiteSpace($Version)) { throw "Version must be stable X.Y.Z." }
    $StrippedVersion = $Version.Trim()
    if ($StrippedVersion.StartsWith("v")) { $StrippedVersion = $StrippedVersion.Substring(1) }
    if ($StrippedVersion -notmatch '^[0-9]+\.[0-9]+\.[0-9]+$') { throw "Version must be stable X.Y.Z (got '$Version')" }
    $Tag = "v$StrippedVersion"
}

$Arch = $env:EGGPASS_TEST_ARCH
if (-not $Arch) { $Arch = $env:PROCESSOR_ARCHITEW6432 }
if (-not $Arch) { $Arch = $env:PROCESSOR_ARCHITECTURE }
if (-not $Arch) { $Arch = "unknown" }
$Target = $null
$SupportedBinary = $false
if (($Arch -eq "AMD64" -or $Arch -eq "x86_64") -and [Environment]::Is64BitOperatingSystem) {
    $Target = "x86_64-pc-windows-msvc"
    $SupportedBinary = $true
}
$Asset = if ($Target) { "eggpass-$Target.exe" } else { $null }
$InstallName = "eggpass.exe"
$IsAdmin = $false
if ([Environment]::OSVersion.Platform -eq [PlatformID]::Win32NT) {
    $IsAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}
Write-Host "Detected: ARCH=$Arch TARGET=$(if ($Target) { $Target } else { 'unknown' }) $(if ($Tag) { $Tag } else { 'latest' })"

function Get-DestDir {
    param([bool]$ForSystem)
    if ($ForSystem) {
        if (-not $script:IsAdmin) { throw "-System requires an already elevated PowerShell session; no elevation is launched." }
        if ($env:EGGPASS_TEST_SYSTEM_DIR) { return $env:EGGPASS_TEST_SYSTEM_DIR }
        return (Join-Path $env:ProgramFiles "Eggpass")
    }
    $local = $env:LOCALAPPDATA
    if (-not $local) { $local = Join-Path $env:USERPROFILE "AppData\Local" }
    return (Join-Path $local "Eggpass")
}

function Test-OnPath {
    param([string]$Dir)
    foreach ($entry in ($env:PATH -split ';')) {
        if ($entry.Trim().Trim('"').TrimEnd('\') -ieq $Dir.TrimEnd('\')) { return $true }
    }
    return $false
}

function Write-PathGuidance {
    param([string]$Dir)
    if (Test-OnPath -Dir $Dir) {
        Write-Host "$Dir is available on the current PATH."
        return
    }
    Write-Warning "$Dir is not in PATH; add it to PATH or use the full executable path."
    Write-Host 'For this PowerShell session, run:'
    Write-Host ('$env:Path = "' + $Dir + ';$env:Path"')
}

function Get-CandidateVersion {
    param([string]$Candidate)
    $output = @(& $Candidate --version 2>&1)
    if ($LASTEXITCODE -ne 0 -or $output.Count -ne 1) { throw "Candidate failed eggpass --version validation: $output" }
    $line = [string]$output[0]
    if ($line -notmatch '^eggpass ([0-9]+\.[0-9]+\.[0-9]+)$') { throw "Candidate identity must be exactly 'eggpass X.Y.Z': $line" }
    return $Matches[1]
}

function Get-ExistingVersion {
    param([string]$DestPath)
    if (-not (Test-Path -LiteralPath $DestPath -PathType Leaf)) { return "" }
    try { return "eggpass $(Get-CandidateVersion -Candidate $DestPath)" } catch { return "" }
}

function Get-DestinationClassification {
    param([string]$DestPath)
    try {
        $item = Get-Item -LiteralPath $DestPath -Force -ErrorAction Stop
    } catch {
        if ($_.CategoryInfo.Category -eq [System.Management.Automation.ErrorCategory]::ObjectNotFound) { return "absent" }
        throw "Cannot safely inspect destination '$DestPath': $($_.Exception.Message)"
    }
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw "Refusing to replace a symlink/reparse point: $DestPath" }
    if ($item.PSIsContainer) { return "foreign" }
    $existing = Get-ExistingVersion -DestPath $DestPath
    if ($existing -match '^eggpass [0-9]+\.[0-9]+\.[0-9]+$') { return "replace" }
    return "foreign"
}

function Invoke-EggpassWebRequest {
    param([string]$Uri, [string]$OutFile, [int]$TimeoutSec)
    Invoke-WebRequest -Uri $Uri -OutFile $OutFile -UseBasicParsing -TimeoutSec $TimeoutSec -MaximumRedirection 5 -ErrorAction Stop
}

function Get-HttpFailure {
    param($ErrorRecord)
    $code = 0
    $responseUri = ""
    $response = $null
    try { $response = $ErrorRecord.Exception.Response } catch { }
    if ($response) {
        $statusProperty = $response.PSObject.Properties['StatusCode']
        if ($statusProperty -and $statusProperty.Value) { $code = [int]$statusProperty.Value }
        $responseUriProperty = $response.PSObject.Properties['ResponseUri']
        if ($responseUriProperty -and $responseUriProperty.Value) {
            $responseUri = [string]$responseUriProperty.Value.AbsoluteUri
        } else {
            $requestProperty = $response.PSObject.Properties['RequestMessage']
            if ($requestProperty -and $requestProperty.Value) {
                $requestUriProperty = $requestProperty.Value.PSObject.Properties['RequestUri']
                if ($requestUriProperty -and $requestUriProperty.Value) { $responseUri = [string]$requestUriProperty.Value.AbsoluteUri }
            }
        }
    }
    return [pscustomobject]@{ StatusCode = $code; ResponseUri = $responseUri }
}

function Resolve-ReleaseVersion {
    param([string]$Uri, [string]$ExpectedAsset)
    if (-not $Uri) { return "" }
    try { $parsed = [Uri]$Uri } catch { return "" }
    if ($parsed.Scheme -ne "https" -or $parsed.Host -ne "github.com" -or $parsed.Query -or $parsed.Fragment) { return "" }
    $parts = $parsed.AbsolutePath.Trim('/').Split('/')
    if ($parts.Count -ne 6 -or $parts[0] -ne "eggstack" -or $parts[1] -ne "eggpass" -or $parts[2] -ne "releases" -or $parts[3] -ne "download") { return "" }
    if ($parts[4] -notmatch '^v([0-9]+\.[0-9]+\.[0-9]+)$') { return "" }
    if ($parts[5] -ne $ExpectedAsset -or $parsed.AbsolutePath -ne "/eggstack/eggpass/releases/download/$($parts[4])/$ExpectedAsset") { return "" }
    return $Matches[1]
}

function Set-PrivateDirectoryAcl {
    param([string]$Path)
    $acl = New-Object System.Security.AccessControl.DirectorySecurity
    $acl.SetAccessRuleProtection($true, $false)
    $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User
    $rule = [System.Security.AccessControl.FileSystemAccessRule]::new($sid, "FullControl", "ContainerInherit,ObjectInherit", "None", "Allow")
    $acl.AddAccessRule($rule)
    Set-Acl -LiteralPath $Path -AclObject $acl
}

function Invoke-EggpassCargo {
    param([string]$CargoPath, [string[]]$Arguments)
    & $CargoPath @Arguments 2>&1 | Out-Host
    return $LASTEXITCODE
}

function Install-VerifiedCandidate {
    param([string]$Candidate, [string]$DestPath, [string]$DestDir, [string]$Scope, [string]$ExistingVersion)
    $dirItem = Get-Item -LiteralPath $DestDir -Force -ErrorAction SilentlyContinue
    if ($dirItem -and ($dirItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw "Refusing to install through a symlink/reparse point: $DestDir" }
    New-Item -ItemType Directory -Path $DestDir -Force | Out-Null
    $dirItem = Get-Item -LiteralPath $DestDir -Force -ErrorAction Stop
    if (($dirItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw "Refusing to install through a symlink/reparse point: $DestDir" }
    $current = Get-DestinationClassification -DestPath $DestPath
    if ($current -ne $Scope) { throw "Destination changed during install (expected $Scope, found $current); refusing to overwrite it." }
    $stagedCopy = Join-Path $DestDir (".eggpass-install-" + [Guid]::NewGuid().ToString('N') + ".tmp")
    $backupPath = $null
    $replacementSucceeded = $false
    try {
        [IO.File]::Copy($Candidate, $stagedCopy, $false)
        if ($Scope -eq "replace") {
            $backupPath = Join-Path $DestDir (".eggpass-backup-" + [Guid]::NewGuid().ToString('N') + ".tmp")
            [IO.File]::Replace($stagedCopy, $DestPath, $backupPath)
            $replacementSucceeded = $true
        }
        else { [IO.File]::Move($stagedCopy, $DestPath) }
    } catch {
        $installError = $_
        if ($backupPath -and [IO.File]::Exists($backupPath) -and -not [IO.File]::Exists($DestPath)) {
            try {
                [IO.File]::Move($backupPath, $DestPath)
                $backupPath = $null
            } catch {
                throw "Installation failed and the prior Eggpass executable could not be restored; its backup remains at $backupPath. Install error: $($installError.Exception.Message). Restore error: $($_.Exception.Message)"
            }
        } elseif ($backupPath -and [IO.File]::Exists($backupPath)) {
            Write-Warning "Replacement failed; retained the prior executable backup at $backupPath for recovery."
        }
        throw $installError
    } finally {
        if (Test-Path -LiteralPath $stagedCopy) { Remove-Item -LiteralPath $stagedCopy -Force -ErrorAction SilentlyContinue }
        if ($replacementSucceeded -and $backupPath -and [IO.File]::Exists($backupPath)) { Remove-Item -LiteralPath $backupPath -Force -ErrorAction SilentlyContinue }
    }
    if ($Scope -eq "replace") { Write-Host "Updated Eggpass at $DestPath ($ExistingVersion -> eggpass $(Get-CandidateVersion -Candidate $DestPath))" }
    else { Write-Host "Installed Eggpass to $DestPath" }
}

function Invoke-CargoFallback {
    param([string]$SelectedVersion, [string]$DestDir, [string]$DestPath, [string]$Scope, [string]$ExistingVersion)
    if ($SelectedVersion -notmatch '^[0-9]+\.[0-9]+\.[0-9]+$') { throw "Cargo fallback requires the exact selected GitHub release version." }
    $cargo = Get-Command cargo -CommandType Application -ErrorAction SilentlyContinue
    if (-not $cargo) { throw "The exact eggpass $SelectedVersion crate is not available through Cargo. Install Rust and retry this exact version later." }
    $stageRoot = Join-Path ([IO.Path]::GetTempPath()) ("eggpass-install-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $stageRoot | Out-Null
    try {
        Set-PrivateDirectoryAcl -Path $stageRoot
        $cargoRoot = Join-Path $stageRoot "cargo-root"
        $cargoArgs = @("install", "--locked", "--version", "=$SelectedVersion", "--root", $cargoRoot, "eggpass")
        Write-Host "No prebuilt asset (HTTP 404); staging exact Eggpass $SelectedVersion with Cargo."
        $exitCode = Invoke-EggpassCargo -CargoPath $cargo.Source -Arguments $cargoArgs
        if ($exitCode -ne 0) { throw "Cargo fallback failed for exact eggpass $SelectedVersion; retry when that crate version is available." }
        $candidate = Join-Path $cargoRoot "bin\eggpass.exe"
        if (-not (Test-Path -LiteralPath $candidate -PathType Leaf)) { throw "Cargo install succeeded but staged candidate is missing: $candidate" }
        $candidateVersion = Get-CandidateVersion -Candidate $candidate
        if ($candidateVersion -ne $SelectedVersion) { throw "Cargo candidate $candidateVersion does not match selected GitHub release $SelectedVersion" }
        Install-VerifiedCandidate -Candidate $candidate -DestPath $DestPath -DestDir $DestDir -Scope $Scope -ExistingVersion $ExistingVersion
    } finally {
        if (Test-Path -LiteralPath $stageRoot) { Remove-Item -LiteralPath $stageRoot -Recurse -Force -ErrorAction SilentlyContinue }
    }
}

function Install-Program {
    if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) { throw "This PowerShell installer runs on Windows only." }
    if (-not $SupportedBinary) { throw "Unsupported target $Arch; no published Eggpass asset exists and Cargo fallback is disabled for unsupported targets." }
    $destDir = Get-DestDir -ForSystem:($System.IsPresent)
    $destDirItem = Get-Item -LiteralPath $destDir -Force -ErrorAction SilentlyContinue
    if ($destDirItem -and ($destDirItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw "Refusing to install through a symlink/reparse point: $destDir" }
    $destPath = Join-Path $destDir $InstallName
    $scope = Get-DestinationClassification -DestPath $destPath
    if ($scope -eq "foreign") { throw "Refusing to overwrite unrelated file at $destPath; it is not an Eggpass executable." }
    $existingVersion = ""
    if ($scope -eq "replace") { $existingVersion = Get-ExistingVersion -DestPath $destPath; Write-Host "Found $existingVersion at $destPath; same-scope rerun will replace it." }

    if ($Tag) { $url = "$BaseUrl/download/$Tag/$Asset" } else { $url = "$BaseUrl/latest/download/$Asset" }
    $shaUrl = "$url.sha256"
    if (-not $url.StartsWith("https://github.com/$Repo/releases/", [StringComparison]::Ordinal)) { throw "Constructed URL is outside the expected Eggpass release repository." }
    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("eggpass-install-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        Set-PrivateDirectoryAcl -Path $tmp
        $candidate = Join-Path $tmp $Asset
        $shaFile = Join-Path $tmp "$Asset.sha256"
        $download = $null
        try {
            $download = Invoke-EggpassWebRequest -Uri $url -OutFile $candidate -TimeoutSec 100
        } catch {
            $failure = Get-HttpFailure -ErrorRecord $_
            if ($failure.StatusCode -ne 404) { throw "Failed to download $Asset (HTTP $($failure.StatusCode)): $($_.Exception.Message)" }
            $selected = $StrippedVersion
            if (-not $selected) { $selected = Resolve-ReleaseVersion -Uri $failure.ResponseUri -ExpectedAsset $Asset }
            if (-not $selected) { throw "Binary asset returned HTTP 404 but no exact release tag could be resolved; refusing floating Cargo fallback." }
            Invoke-CargoFallback -SelectedVersion $selected -DestDir $destDir -DestPath $destPath -Scope $scope -ExistingVersion $existingVersion
            Write-PathGuidance -Dir $destDir
            return
        }
        if (-not (Test-Path -LiteralPath $candidate -PathType Leaf)) { throw "Download completed without an asset file." }
        $assetLength = (Get-Item -LiteralPath $candidate).Length
        if ($assetLength -gt 67108864) { throw "Asset exceeds the 67108864 byte maximum." }

        try { Invoke-EggpassWebRequest -Uri $shaUrl -OutFile $shaFile -TimeoutSec 30 | Out-Null }
        catch { throw "Failed to download checksum for ${Asset}: $($_.Exception.Message)" }
        if (-not (Test-Path -LiteralPath $shaFile -PathType Leaf) -or (Get-Item -LiteralPath $shaFile).Length -gt 4096) { throw "Checksum sidecar is missing or exceeds 4096 bytes." }
        $expected = ((Get-Content -LiteralPath $shaFile -Raw).Split())[0].ToLowerInvariant()
        if ($expected -notmatch '^[0-9a-f]{64}$') { throw "Checksum sidecar is malformed." }
        $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $candidate).Hash.ToLowerInvariant()
        if ($expected -ne $actual) { throw "SHA-256 mismatch for $Asset." }

        $candidateVersion = Get-CandidateVersion -Candidate $candidate
        if ($StrippedVersion -and $candidateVersion -ne $StrippedVersion) { throw "Candidate version $candidateVersion does not match requested $StrippedVersion." }
        Install-VerifiedCandidate -Candidate $candidate -DestPath $destPath -DestDir $destDir -Scope $scope -ExistingVersion $existingVersion
        Write-PathGuidance -Dir $destDir
    } finally {
        if (Test-Path -LiteralPath $tmp) { Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue }
    }
}

if ($env:EGGPASS_TEST_SKIP_MAIN -ne "1") { Install-Program }
