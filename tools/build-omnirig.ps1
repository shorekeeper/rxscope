#Requires -Version 7.0

[CmdletBinding()]
param(
    [ValidateSet('both', 'x64', 'x86')]
    [string] $Arch = 'both',

    [ValidateSet('release', 'debug')]
    [string] $Configuration = 'release',

    # Removes the registration for the selected architectures and builds nothing.
    [switch] $Remove,

    # Registers whatever is already built.
    [switch] $NoBuild,

    # Root of the Visual Studio installation. Resolved automatically when absent.
    [string] $VsPath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# A non zero exit code from a native command throws by default in recent
# releases. Every exit code here is examined and reported with the step it
# belongs to, so the automatic behaviour is switched off rather than worked
# around with try blocks around each call.
$PSNativeCommandUseErrorActionPreference = $false

# ----------------------------------------------------------------- constants

$Package = 'detent-omnirig'
$Module  = 'detent_omnirig.dll'

# Class identifier of the object, from guid.rs. Held here so registration can be
# verified in the registry rather than inferred from an exit code.
$Clsid = '{36A2747E-A6DF-4E86-A08F-2F88CEA7C7DA}'

# Name the established object uses. Claimed only when free, so whether it points
# here is worth reporting: a client asking for it by name is otherwise served by
# whatever holds it.
$SharedProgId = 'OmniRig.OmniRigX'

$Triples = @{
    x64 = 'x86_64-pc-windows-msvc'
    x86 = 'i686-pc-windows-msvc'
}

$VcArchOf = @{
    x64 = 'x64'
    x86 = 'x86'
}

# Machine field of the PE header, from the file itself.
$MachineNames = @{
    0x8664 = 'x64'
    0x014C = 'x86'
    0xAA64 = 'arm64'
}

# ------------------------------------------------------------------- output

function Write-Step {
    param([string] $Text)
    Write-Host ''
    Write-Host "== $Text" -ForegroundColor Cyan
}

function Write-Field {
    param([string] $Name, $Value)
    Write-Host ('  {0,-16} {1}' -f $Name, $Value)
}

function Write-Problem {
    param([string] $Text)
    Write-Host "  $Text" -ForegroundColor Yellow
}

# ------------------------------------------------------------------- layout

function Get-RepoRoot {
    # The script lives under tools, so the workspace is one level up. Verified
    # rather than assumed, because a copy placed elsewhere would otherwise build
    # nothing and report success.
    $candidate = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
    $manifest = Join-Path $candidate 'Cargo.toml'
    if (-not (Test-Path -LiteralPath $manifest)) {
        throw "no Cargo.toml at $candidate"
    }
    if (-not (Select-String -LiteralPath $manifest -Pattern '^\s*\[workspace\]' -Quiet)) {
        throw "$manifest is not the workspace manifest"
    }
    return $candidate
}

function Get-OutputPath {
    param([string] $Root, [string] $Triple, [string] $Configuration)
    return Join-Path $Root "target\$Triple\$Configuration\$Module"
}

# --------------------------------------------------------------- toolchain

function Get-VcVarsPath {
    param([string] $Requested)

    $roots = New-Object System.Collections.Generic.List[string]

    if ($Requested) {
        $roots.Add($Requested)
    }

    # Set inside a developer prompt, in which case the answer is already known.
    if ($env:VSINSTALLDIR) {
        $roots.Add($env:VSINSTALLDIR)
    }

    # The locator itself is always at a fixed path, whatever the installation is.
    # Prerelease instances are excluded from the default query, which is the
    # whole reason the switch is passed.
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (Test-Path -LiteralPath $vswhere) {
        $found = & $vswhere -prerelease -latest `
            -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
            -property installationPath 2>$null
        foreach ($line in @($found)) {
            if ($line) { $roots.Add($line.Trim()) }
        }
    }

    foreach ($edition in @('Insiders', 'Preview', 'Enterprise', 'Professional', 'Community')) {
        foreach ($version in @('18', '2026', '17', '2022')) {
            $roots.Add((Join-Path $env:ProgramFiles "Microsoft Visual Studio\$version\$edition"))
        }
    }

    foreach ($root in $roots) {
        if (-not $root) { continue }
        $path = Join-Path $root 'VC\Auxiliary\Build\vcvarsall.bat'
        if (Test-Path -LiteralPath $path) {
            return $path
        }
    }

    throw 'vcvarsall.bat not found. Pass -VsPath with the installation root.'
}

function Install-RustTarget {
    param([string] $Triple)

    $rustup = Get-Command rustup -ErrorAction SilentlyContinue
    if (-not $rustup) {
        # A standalone toolchain has no target management and may already carry
        # the target, so this is reported and not treated as a failure.
        Write-Problem 'rustup not found, assuming the target is present'
        return
    }

    $installed = & rustup target list --installed 2>$null
    if ($LASTEXITCODE -eq 0) {
        foreach ($line in @($installed)) {
            if ($line -and $line.Trim() -eq $Triple) {
                Write-Field 'target' "$Triple present"
                return
            }
        }
    }

    Write-Field 'target' "$Triple missing, installing"
    & rustup target add $Triple
    if ($LASTEXITCODE -ne 0) {
        throw "rustup target add $Triple failed with $LASTEXITCODE"
    }
}

function Invoke-CargoBuild {
    param(
        [string] $Root,
        [string] $VcVars,
        [string] $VcArch,
        [string] $Triple,
        [string] $Configuration
    )

    $flag = if ($Configuration -eq 'release') { '--release' } else { '' }

    # A temporary script rather than a compound command line. The interpreter and
    # this shell disagree about quoting, and a path holding a space is enough for
    # the disagreement to matter.
    $lines = @(
        '@echo off'
        "call `"$VcVars`" $VcArch >nul"
        'if errorlevel 1 exit /b 1'
        "cd /d `"$Root`""
        "cargo build $flag -p $Package --target $Triple"
        'exit /b %errorlevel%'
    )

    $file = Join-Path ([System.IO.Path]::GetTempPath()) ("detent-build-{0}.cmd" -f [guid]::NewGuid().ToString('N'))
    # Batch files are read in the console code page, so the encoding is chosen to
    # match rather than left at the shell default.
    Set-Content -LiteralPath $file -Value $lines -Encoding oem

    try {
        & cmd.exe /c $file
        return $LASTEXITCODE
    }
    finally {
        Remove-Item -LiteralPath $file -ErrorAction SilentlyContinue
    }
}

# ------------------------------------------------------------------- module

function Get-PeMachine {
    param([string] $Path)

    # Read from the header rather than obtained by loading the file: loading a
    # module of the wrong word size is precisely what fails, and this check
    # exists to catch that before the registration utility is chosen.
    $stream = [System.IO.File]::OpenRead($Path)
    try {
        $reader = New-Object System.IO.BinaryReader($stream)
        $stream.Position = 0x3C
        $headerAt = $reader.ReadInt32()
        $stream.Position = $headerAt + 4
        $machine = $reader.ReadUInt16()
    }
    finally {
        $stream.Dispose()
    }

    if ($MachineNames.ContainsKey([int]$machine)) {
        return $MachineNames[[int]$machine]
    }
    return ('unknown 0x{0:X4}' -f $machine)
}

function Copy-RigDescriptions {
    param([string] $Root, [string] $ModulePath)

    # The default description directory is a subdirectory beside the module. The
    # build output is not that place, so the files are copied there: without them
    # the object registers, creates and then reports that no description exists.
    $source = Join-Path $Root 'rigs'
    if (-not (Test-Path -LiteralPath $source)) {
        return 0
    }

    $files = Get-ChildItem -LiteralPath $source -Filter '*.ini' -File -ErrorAction SilentlyContinue
    if (-not $files) {
        return 0
    }

    # The directory name is taken from the string rather than through Split-Path.
    # That cmdlet accepts a mode switch only in its Path parameter set, so
    # -LiteralPath together with -Parent resolves to no set at all; the framework
    # call also leaves wildcard characters in a path alone, which a provider based
    # one does not.
    $parent = [System.IO.Path]::GetDirectoryName($ModulePath)
    if (-not $parent) {
        return 0
    }

    $destination = Join-Path $parent 'rigs'
    New-Item -ItemType Directory -Force -Path $destination | Out-Null
    foreach ($file in $files) {
        Copy-Item -LiteralPath $file.FullName -Destination $destination -Force
    }
    return $files.Count
}

# ------------------------------------------------------------- registration

function Get-Regsvr32Path {
    param([ValidateSet('x64', 'x86')][string] $Arch)

    $win = $env:WINDIR
    $wow = Join-Path $win 'SysWOW64'

    # The naming reads as the reverse of what it is: the native directory holds
    # the utility of the host word size and SysWOW64 holds the thirty two bit
    # one. A thirty two bit host reaches the native directory only under the
    # Sysnative alias, because System32 is redirected for it.
    $native = if ([Environment]::Is64BitProcess -or -not [Environment]::Is64BitOperatingSystem) {
        Join-Path $win 'System32'
    } else {
        Join-Path $win 'Sysnative'
    }

    $path = if ($Arch -eq 'x86' -and (Test-Path -LiteralPath $wow)) {
        Join-Path $wow 'regsvr32.exe'
    } elseif ($Arch -eq 'x86') {
        # A thirty two bit operating system has one utility and it is the right one.
        Join-Path $win 'System32\regsvr32.exe'
    } else {
        Join-Path $native 'regsvr32.exe'
    }

    if (-not (Test-Path -LiteralPath $path)) {
        throw "regsvr32 for $Arch not found at $path"
    }
    return $path
}

function Invoke-Regsvr32 {
    param([string] $Tool, [string] $ModulePath, [switch] $Unregister)

    $arguments = @('/s')
    if ($Unregister) { $arguments += '/u' }
    $arguments += "`"$ModulePath`""

    # The utility belongs to the graphical subsystem, so the shell does not wait
    # for it and the exit code is only available through an explicit wait.
    $process = Start-Process -FilePath $Tool -ArgumentList $arguments -Wait -PassThru
    return $process.ExitCode
}

function Get-RegistrationInfo {
    param([ValidateSet('x64', 'x86')][string] $Arch)

    # The view is stated rather than inherited from the host, so the answer does
    # not depend on which shell this runs in.
    $view = if ($Arch -eq 'x64') {
        [Microsoft.Win32.RegistryView]::Registry64
    } else {
        [Microsoft.Win32.RegistryView]::Registry32
    }

    $result = [pscustomobject]@{
        Server = $null
        SharedProgId = $null
    }

    $hive = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::CurrentUser, $view)
    try {
        $key = $hive.OpenSubKey("Software\Classes\CLSID\$Clsid\InprocServer32")
        if ($key) {
            try { $result.Server = $key.GetValue('') } finally { $key.Dispose() }
        }

        $progid = $hive.OpenSubKey("Software\Classes\$SharedProgId\CLSID")
        if ($progid) {
            try { $result.SharedProgId = $progid.GetValue('') } finally { $progid.Dispose() }
        }
    }
    finally {
        $hive.Dispose()
    }

    return $result
}

function Show-RegisterLog {
    param([int] $Lines = 30)

    # The object records what it did, because the utility reports one code and
    # the object has no logger of its own inside somebody else's process. On a
    # partial failure this names the key that could not be written.
    $log = Join-Path $env:APPDATA 'Detent\register.log'
    if (-not (Test-Path -LiteralPath $log)) {
        return
    }
    Write-Step "log $log"
    Get-Content -LiteralPath $log -Tail $Lines | ForEach-Object { Write-Host "  $_" }
}

# --------------------------------------------------------------------- main

$root = Get-RepoRoot
$selected = if ($Arch -eq 'both') { @('x64', 'x86') } else { @($Arch) }
$build = -not ($Remove -or $NoBuild)

Write-Step 'plan'
Write-Field 'workspace' $root
Write-Field 'package' $Package
Write-Field 'configuration' $Configuration
Write-Field 'architectures' ($selected -join ', ')
Write-Field 'action' $(if ($Remove) { 'unregister' } elseif ($NoBuild) { 'register only' } else { 'build and register' })

$vcvars = $null
if ($build) {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        throw 'cargo not found on the path'
    }
    $vcvars = Get-VcVarsPath -Requested $VsPath
    Write-Field 'vcvarsall' $vcvars
}

$report = New-Object System.Collections.Generic.List[object]
$failures = 0

foreach ($arch in $selected) {
    $triple = $Triples[$arch]
    $modulePath = Get-OutputPath -Root $root -Triple $triple -Configuration $Configuration

    Write-Step "$arch  $triple"

    if ($build) {
        Install-RustTarget -Triple $triple

        $code = Invoke-CargoBuild -Root $root -VcVars $vcvars -VcArch $VcArchOf[$arch] `
            -Triple $triple -Configuration $Configuration
        if ($code -ne 0) {
            Write-Problem "build failed with $code"
            $failures++
            $report.Add([pscustomobject]@{
                Arch = $arch; Machine = ''; Module = $modulePath; Rigs = 0
                Result = "build failed ($code)"; Registered = ''
            })
            continue
        }
    }

    if (-not (Test-Path -LiteralPath $modulePath)) {
        Write-Problem "not found: $modulePath"
        $failures++
        $report.Add([pscustomobject]@{
            Arch = $arch; Machine = ''; Module = $modulePath; Rigs = 0
            Result = 'module missing'; Registered = ''
        })
        continue
    }

    $machine = Get-PeMachine -Path $modulePath
    Write-Field 'module' $modulePath
    Write-Field 'machine' $machine

    if ($machine -ne $arch) {
        # Registering it anyway would place the class where nothing can load it,
        # and nothing later would report that, so this stops here.
        Write-Problem "expected $arch, refusing to register"
        $failures++
        $report.Add([pscustomobject]@{
            Arch = $arch; Machine = $machine; Module = $modulePath; Rigs = 0
            Result = 'word size mismatch'; Registered = ''
        })
        continue
    }

    $rigs = 0
    if (-not $Remove) {
        $rigs = Copy-RigDescriptions -Root $root -ModulePath $modulePath
        Write-Field 'descriptions' "$rigs copied"
    }

    $tool = Get-Regsvr32Path -Arch $arch
    Write-Field 'utility' $tool

    $code = Invoke-Regsvr32 -Tool $tool -ModulePath $modulePath -Unregister:$Remove
    $action = if ($Remove) { 'unregistered' } else { 'registered' }
    if ($code -ne 0) {
        Write-Problem "$action failed with $code"
        $failures++
    } else {
        Write-Field 'result' $action
    }

    $info = Get-RegistrationInfo -Arch $arch
    $registered = if ($info.Server) { $info.Server } else { 'absent' }
    Write-Field 'class points at' $registered

    if (-not $Remove) {
        if ($info.SharedProgId -and $info.SharedProgId -eq $Clsid) {
            Write-Field $SharedProgId 'points here'
        } elseif ($info.SharedProgId) {
            # Not an error. The name is claimed only when free, so an established
            # installation keeps it and a client asking for it by name is served
            # by that installation rather than by this object.
            Write-Field $SharedProgId "held by $($info.SharedProgId)"
        } else {
            Write-Field $SharedProgId 'not claimed'
        }
    }

    $report.Add([pscustomobject]@{
        Arch = $arch
        Machine = $machine
        Module = $modulePath
        Rigs = $rigs
        Result = $(if ($code -eq 0) { $action } else { "$action failed ($code)" })
        Registered = $registered
    })
}

Show-RegisterLog

Write-Step 'summary'
$report | Format-Table -Property Arch, Machine, Result, Rigs, Module -AutoSize

if (-not $Remove) {
    $configFile = Join-Path $env:APPDATA 'Detent\omnirig.ini'
    Write-Field 'configuration' $configFile
    if (Test-Path -LiteralPath $configFile) {
        Write-Field 'state' 'present, values are not touched by this script'
    } else {
        Write-Field 'state' 'written on the first creation of the object'
    }
    Write-Host ''
    Write-Host '  reach the object as Detent.OmniRigX, or by class identifier'
    Write-Host '  late binding only: the established interface identifiers are refused'
}

if ($failures -gt 0) {
    Write-Host ''
    Write-Host "$failures step(s) failed" -ForegroundColor Red
    exit 1
}
exit 0