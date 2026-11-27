$ErrorActionPreference = "Stop"
$installer = Join-Path $PSScriptRoot "../install.ps1"
$installer = (Resolve-Path $installer).Path
$env:EGGPASS_TEST_SKIP_MAIN = "1"
$env:EGGPASS_TEST_ARCH = "AMD64"
. $installer

if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    Write-Host "PowerShell runtime fixtures require Windows; URL/helper fixtures are ready for Windows PowerShell 7."
    if ((Resolve-ReleaseVersion -Uri "https://github.com/eggstack/eggpass/releases/download/v1.2.3/eggpass-x86_64-pc-windows-msvc.exe" -ExpectedAsset "eggpass-x86_64-pc-windows-msvc.exe") -ne "1.2.3") { throw "release URL fixture failed" }
    exit 0
}

$script:Sandbox = Join-Path ([IO.Path]::GetTempPath()) ("eggpass-installer-tests-" + [Guid]::NewGuid().ToString('N'))
$script:FakeBin = Join-Path $script:Sandbox "fakebin"
$script:FixtureBinary = Join-Path $script:Sandbox "fixture.exe"
New-Item -ItemType Directory -Path $script:Sandbox, $script:FakeBin -Force | Out-Null
[IO.File]::WriteAllText($script:FixtureBinary, "offline fixture bytes")
$env:LOCALAPPDATA = Join-Path $script:Sandbox "local"
$env:USERPROFILE = Join-Path $script:Sandbox "profile"
$env:TEMP = Join-Path $script:Sandbox "tmp"
New-Item -ItemType Directory -Path $env:LOCALAPPDATA, $env:USERPROFILE, $env:TEMP -Force | Out-Null
$oldPath = $env:PATH
$env:PATH = "$script:FakeBin;$oldPath"
[IO.File]::WriteAllText((Join-Path $script:FakeBin "cargo.cmd"), "@echo off" + [Environment]::NewLine + "exit /b 0" + [Environment]::NewLine)
$script:FixtureMode = "ok"
$script:FixtureVersion = "1.2.3"
$script:FixtureUrls = [System.Collections.Generic.List[string]]::new()
$script:CargoArgsSeen = [System.Collections.Generic.List[string]]::new()
$script:CandidateVersions = @{}
$script:CargoRootSeen = ""

function Assert-Fixture {
    param([bool]$Condition, [string]$Name)
    if (-not $Condition) { throw "Fixture failed: $Name" }
    Write-Host "ok - $Name"
}
function Reset-Fixture {
    param([string]$Name)
    $script:FixtureMode = "ok"
    $script:FixtureVersion = "1.2.3"
    $script:FixtureUrls.Clear()
    $script:CargoArgsSeen.Clear()
    $script:CandidateVersions.Clear()
    $script:CargoRootSeen = ""
    $script:DestRoot = Join-Path $env:LOCALAPPDATA "Eggpass"
    if (Test-Path -LiteralPath $script:DestRoot) { Remove-Item -LiteralPath $script:DestRoot -Recurse -Force }
    New-Item -ItemType Directory -Path $script:DestRoot -Force | Out-Null
    $script:StrippedVersion = ""
    $script:Tag = ""
    $script:System = [System.Management.Automation.SwitchParameter]::new($false)
}
function Get-CandidateVersion {
    param([string]$Candidate)
    if ($script:CandidateVersions.ContainsKey($Candidate)) { return [string]$script:CandidateVersions[$Candidate] }
    return [string]$script:FixtureVersion
}
function Set-PrivateDirectoryAcl {
    param([string]$Path)
}
function Invoke-EggpassWebRequest {
    param([string]$Uri, [string]$OutFile, [int]$TimeoutSec)
    if ($script:Tag) { $expectedUrl = "https://github.com/eggstack/eggpass/releases/download/$script:Tag/$Asset" }
    else { $expectedUrl = "https://github.com/eggstack/eggpass/releases/latest/download/$Asset" }
    if ([string]::Equals($Uri, $expectedUrl, [StringComparison]::Ordinal)) { $isChecksum = $false }
    elseif ([string]::Equals($Uri, "$expectedUrl.sha256", [StringComparison]::Ordinal)) { $isChecksum = $true }
    else { throw "Offline fixture refused unexpected host/path: $Uri" }
    $script:FixtureUrls.Add($Uri)
    if ($isChecksum) {
        if ($script:FixtureMode -eq "sha-error") { throw [Exception]::new("fixture checksum transport failure") }
        if ($script:FixtureMode -eq "malformed-sha") { [IO.File]::WriteAllText($OutFile, "not-a-digest"); return }
        $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $script:FixtureBinary).Hash.ToLowerInvariant()
        if ($script:FixtureMode -eq "wrong-sha") { $hash = "0" * 64 }
        [IO.File]::WriteAllText($OutFile, "$hash  eggpass-x86_64-pc-windows-msvc.exe")
        return
    }
    if ($script:FixtureMode -eq "transport") { throw [Exception]::new("fixture network failure") }
    if ($script:FixtureMode -eq "http500") { Throw-FixtureHttp -Status 500 -Uri $Uri }
    if ($script:FixtureMode -eq "404") {
        $finalUri = "https://github.com/eggstack/eggpass/releases/download/v1.2.3/eggpass-x86_64-pc-windows-msvc.exe"
        Throw-FixtureHttp -Status 404 -Uri $finalUri
    }
    Copy-Item -LiteralPath $script:FixtureBinary -Destination $OutFile -Force
    $script:CandidateVersions[$OutFile] = $script:FixtureVersion
}
function Throw-FixtureHttp {
    param([int]$Status, [string]$Uri)
    $ex = [Exception]::new("fixture HTTP $Status")
    $response = [pscustomobject]@{ StatusCode = $Status; ResponseUri = [Uri]$Uri; RequestMessage = $null }
    $ex | Add-Member -MemberType NoteProperty -Name Response -Value $response
    throw $ex
}
function Invoke-EggpassCargo {
    param([string]$CargoPath, [string[]]$Arguments)
    $expectedVersion = if ($script:Tag) { $script:StrippedVersion } else { $script:FixtureVersion }
    if ($Arguments.Count -ne 7 -or $Arguments[0] -cne "install" -or $Arguments[1] -cne "--locked" -or $Arguments[2] -cne "--version" -or $Arguments[3] -cne "=$expectedVersion" -or $Arguments[4] -cne "--root" -or $Arguments[6] -cne "eggpass") {
        throw "Offline fixture refused unexpected Cargo arguments: $($Arguments -join ' ')"
    }
    $script:CargoRootSeen = $Arguments[5]
    $tempPrefix = Join-Path ([IO.Path]::GetTempPath()) "eggpass-install-"
    if (-not $script:CargoRootSeen.StartsWith($tempPrefix, [StringComparison]::OrdinalIgnoreCase) -or (Split-Path -Leaf $script:CargoRootSeen) -cne "cargo-root") {
        throw "Offline fixture refused unexpected Cargo staging root: $script:CargoRootSeen"
    }
    foreach ($arg in $Arguments) { $script:CargoArgsSeen.Add($arg) }
    $requested = $Arguments[3].TrimStart('=')
    $binDir = Join-Path $script:CargoRootSeen "bin"
    New-Item -ItemType Directory -Path $binDir -Force | Out-Null
    $staged = Join-Path $binDir "eggpass.exe"
    Copy-Item -LiteralPath $script:FixtureBinary -Destination $staged -Force
    [IO.File]::WriteAllText((Join-Path $script:CargoRootSeen ".crates.toml"), "fake metadata")
    $script:CandidateVersions[$staged] = $requested
    return 0
}

try {
    Assert-Fixture ($Target -eq "x86_64-pc-windows-msvc" -and $Asset -eq "eggpass-x86_64-pc-windows-msvc.exe") "Windows target maps to distribution asset"
    Assert-Fixture ((Resolve-ReleaseVersion -Uri "https://github.com/eggstack/eggpass/releases/download/v1.2.3/eggpass-x86_64-pc-windows-msvc.exe" -ExpectedAsset $Asset) -eq "1.2.3") "latest 404 URL resolves exact release version"
    Assert-Fixture (-not (Resolve-ReleaseVersion -Uri "https://github.com/eggstack/eggpass/releases/latest/download/$Asset" -ExpectedAsset $Asset)) "floating URL is not accepted as exact version"

    Reset-Fixture "latest"
    $script:System = [System.Management.Automation.SwitchParameter]::new($false)
    Install-Program
    Assert-Fixture (Test-Path -LiteralPath (Join-Path $script:DestRoot "eggpass.exe")) "latest fixture installs only into isolated user directory"
    Assert-Fixture ($script:FixtureUrls.Count -eq 2 -and $script:FixtureUrls[0] -ceq "https://github.com/eggstack/eggpass/releases/latest/download/$Asset" -and $script:FixtureUrls[1] -ceq "https://github.com/eggstack/eggpass/releases/latest/download/$Asset.sha256") "latest binary/checksum URLs use GitHub latest authority"

    Reset-Fixture "pinned"
    $script:StrippedVersion = "2.3.4"; $script:Tag = "v2.3.4"; $script:FixtureVersion = "2.3.4"
    Install-Program
    Assert-Fixture ($script:FixtureUrls.Count -eq 2 -and $script:FixtureUrls[0] -ceq "https://github.com/eggstack/eggpass/releases/download/v2.3.4/$Asset" -and $script:FixtureUrls[1] -ceq "https://github.com/eggstack/eggpass/releases/download/v2.3.4/$Asset.sha256") "pin binary/checksum URLs use exact v2.3.4 tag"

    Reset-Fixture "checksum-failure"
    $script:FixtureMode = "wrong-sha"
    try { Install-Program; throw "checksum failure was accepted" } catch { Assert-Fixture (-not (Test-Path -LiteralPath (Join-Path $script:DestRoot "eggpass.exe"))) "checksum mismatch never installs" }
    Assert-Fixture ($script:CargoArgsSeen.Count -eq 0) "checksum failure never triggers Cargo"

    Reset-Fixture "malformed-checksum"
    $script:FixtureMode = "malformed-sha"
    $malformedFailed = $false
    try { Install-Program } catch { $malformedFailed = $true }
    Assert-Fixture ($malformedFailed -and -not (Test-Path -LiteralPath (Join-Path $script:DestRoot "eggpass.exe"))) "malformed checksum never installs"
    Assert-Fixture ($script:CargoArgsSeen.Count -eq 0) "malformed checksum never triggers Cargo"

    Reset-Fixture "identity-failure"
    $script:StrippedVersion = "1.2.3"; $script:Tag = "v1.2.3"; $script:FixtureVersion = "9.9.9"
    try { Install-Program; throw "identity mismatch was accepted" } catch { Assert-Fixture (-not (Test-Path -LiteralPath (Join-Path $script:DestRoot "eggpass.exe"))) "identity mismatch never installs" }
    Assert-Fixture ($script:CargoArgsSeen.Count -eq 0) "identity failure never triggers Cargo"

    Reset-Fixture "transport-failure"
    $script:FixtureMode = "transport"
    $transportFailed = $false
    try { Install-Program } catch { $transportFailed = $true }
    Assert-Fixture ($transportFailed -and $script:CargoArgsSeen.Count -eq 0) "transport failure never triggers Cargo"

    Reset-Fixture "fallback"
    $script:FixtureMode = "404"
    Install-Program
    Assert-Fixture ($script:CargoArgsSeen.Contains("--version") -and $script:CargoArgsSeen.Contains("=1.2.3")) "exact 404 Cargo fallback pins selected release"
    $cargoStageParent = Split-Path -Parent $script:CargoRootSeen
    Assert-Fixture (-not (Test-Path -LiteralPath $script:CargoRootSeen) -and -not (Test-Path -LiteralPath $cargoStageParent)) "Cargo staging root and parent are removed"
    Assert-Fixture (Test-Path -LiteralPath (Join-Path $script:DestRoot "eggpass.exe")) "Cargo candidate is copied to installer-owned destination"

    Reset-Fixture "http500-failure"
    $script:FixtureMode = "http500"
    $serverFailed = $false
    try { Install-Program } catch { $serverFailed = $true }
    Assert-Fixture ($serverFailed -and $script:CargoArgsSeen.Count -eq 0) "non-404 HTTP failure never triggers Cargo"

    Reset-Fixture "same-scope-rerun"
    Install-Program
    $rerunPath = Join-Path $script:DestRoot "eggpass.exe"
    $script:CandidateVersions[$rerunPath] = "1.2.3"
    $script:FixtureVersion = "1.2.4"
    Install-Program
    $script:CandidateVersions[$rerunPath] = "1.2.4"
    Assert-Fixture ((Get-DestinationClassification -DestPath $rerunPath) -eq "replace" -and (Get-ExistingVersion -DestPath $rerunPath) -eq "eggpass 1.2.4") "same-scope rerun replaces the verified Eggpass executable"
    Assert-Fixture ($script:FixtureUrls.Count -eq 4) "same-scope rerun fetches both exact latest assets again"
    $replacementTemps = @(Get-ChildItem -LiteralPath $script:DestRoot -Filter ".eggpass-*.tmp" -ErrorAction SilentlyContinue)
    Assert-Fixture ($replacementTemps.Count -eq 0) "same-scope replacement cleans staged and backup files"

    Reset-Fixture "foreign"
    $foreign = Join-Path $script:DestRoot "eggpass.exe"
    [IO.File]::WriteAllText($foreign, "foreign")
    $script:CandidateVersions[$foreign] = "not-a-version"
    try { Install-Program; throw "foreign destination was accepted" } catch { Assert-Fixture ($script:FixtureUrls.Count -eq 0) "foreign executable is refused before transport" }

    if (-not $IsAdmin) {
        $systemDenied = $false
        try { Get-DestDir -ForSystem:$true } catch { $systemDenied = $true }
        Assert-Fixture $systemDenied "system destination requires elevation"
    } else {
        $env:EGGPASS_TEST_SYSTEM_DIR = Join-Path $script:Sandbox "system-destination"
        Assert-Fixture ((Get-DestDir -ForSystem:$true) -eq $env:EGGPASS_TEST_SYSTEM_DIR) "explicit system destination is separately scoped"
    }

    Reset-Fixture "reparse-point"
    $outsideTarget = Join-Path $script:Sandbox "reparse-target"
    New-Item -ItemType Directory -Path $outsideTarget -Force | Out-Null
    Remove-Item -LiteralPath $script:DestRoot -Recurse -Force
    New-Item -ItemType Junction -Path $script:DestRoot -Target $outsideTarget | Out-Null
    $reparseRefused = $false
    try { Install-Program } catch { $reparseRefused = $true }
    Assert-Fixture ($reparseRefused -and $script:FixtureUrls.Count -eq 0 -and -not (Test-Path -LiteralPath (Join-Path $outsideTarget "eggpass.exe"))) "reparse-point destination is refused before transport"
    Remove-Item -LiteralPath $script:DestRoot -Force

    $oldArch = $script:Arch
    $oldSupported = $script:SupportedBinary
    $script:Arch = "ARM64"
    $script:SupportedBinary = $false
    Reset-Fixture "unsupported-architecture"
    $unsupportedRefused = $false
    try { Install-Program } catch { $unsupportedRefused = $true }
    Assert-Fixture ($unsupportedRefused -and $script:FixtureUrls.Count -eq 0 -and $script:CargoArgsSeen.Count -eq 0) "unsupported architecture refuses release and Cargo access"
    $script:Arch = $oldArch
    $script:SupportedBinary = $oldSupported
    Write-Host "PowerShell fixture suite passed."
} finally {
    $env:PATH = $oldPath
    Remove-Item Env:EGGPASS_TEST_SKIP_MAIN -ErrorAction SilentlyContinue
    Remove-Item Env:EGGPASS_TEST_ARCH -ErrorAction SilentlyContinue
    if (Test-Path -LiteralPath $script:Sandbox) { Remove-Item -LiteralPath $script:Sandbox -Recurse -Force -ErrorAction SilentlyContinue }
}
