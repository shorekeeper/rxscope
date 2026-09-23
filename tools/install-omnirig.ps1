# Registers or removes the object, for the correct word size.
#
#   .\install-omnirig.ps1 -Path ..\target\release\detent_omnirig.dll
#   .\install-omnirig.ps1 -Path ..\target\i686-pc-windows-msvc\release\detent_omnirig.dll
#   .\install-omnirig.ps1 -Path ... -Remove
#
# The word size is read from the file rather than asked for. Handing a thirty
# two bit module to the sixty four bit registration utility succeeds and
# registers it in the wrong view, where no process of either size can load it;
# that is the one failure this script exists to prevent.

param(
    [Parameter(Mandatory = $true)]
    [string] $Path,
    [switch] $Remove
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path -LiteralPath $Path)) {
    Write-Host "not found: $Path"
    exit 1
}
$module = (Resolve-Path -LiteralPath $Path).Path

# The machine field of the header. Read directly because the alternative is a
# load, and loading a module of the wrong size is exactly what fails.
$stream = [System.IO.File]::OpenRead($module)
try {
    $reader = New-Object System.IO.BinaryReader($stream)
    $stream.Position = 0x3C
    $headerAt = $reader.ReadInt32()
    $stream.Position = $headerAt + 4
    $machine = $reader.ReadUInt16()
} finally {
    $stream.Dispose()
}

switch ($machine) {
    0x8664 { $bits = 64 }
    0x014C { $bits = 32 }
    default {
        Write-Host ("unrecognized machine 0x{0:X4}" -f $machine)
        exit 1
    }
}

$system = [Environment]::GetFolderPath("System")
$wow    = Join-Path $env:WINDIR "SysWOW64"

# On a sixty four bit system the native directory holds the sixty four bit
# utility and SysWOW64 holds the thirty two bit one. The naming is the reverse
# of what it reads as and is a frequent source of a registration that lands
# nowhere.
if ($bits -eq 64) {
    $tool = Join-Path $system "regsvr32.exe"
} else {
    $tool = if (Test-Path $wow) { Join-Path $wow "regsvr32.exe" } else { Join-Path $system "regsvr32.exe" }
}

Write-Host "module   $module"
Write-Host "word     $bits bit"
Write-Host "utility  $tool"

$args = @("/s")
if ($Remove) { $args += "/u" }
$args += "`"$module`""

$process = Start-Process -FilePath $tool -ArgumentList $args -Wait -PassThru
if ($process.ExitCode -ne 0) {
    Write-Host "failed, exit code $($process.ExitCode)"
} else {
    Write-Host $(if ($Remove) { "removed" } else { "registered" })
}

# The object records what it did, because the utility reports one code and the
# object has no logger of its own inside somebody else's process.
$log = Join-Path $env:APPDATA "Detent\register.log"
if (Test-Path -LiteralPath $log) {
    Write-Host ""
    Write-Host "last entries from $log"
    Get-Content -LiteralPath $log -Tail 40 | ForEach-Object { Write-Host "  $_" }
}

exit $process.ExitCode