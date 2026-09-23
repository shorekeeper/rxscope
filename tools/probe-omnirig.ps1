# Reads the properties of an OmniRig compatible object.
#
#   .\probe-omnirig.ps1                     the established object
#   .\probe-omnirig.ps1 Detent.OmniRigX     ours
#   .\probe-omnirig.ps1 "{74E87CF5-F8CE-4C38-82AE-1673839DB15F}"
#
# The identifier form is needed for the version two object, which registers no
# name and can only be reached that way.
#
# Run from a sixty four bit shell. A thirty two bit in process object cannot be
# reached from one, but the established object is a local server and is reached
# through the platform marshaller, so both work.

param(
    [string] $Target = "OmniRig.OmniRigX"
)

$ErrorActionPreference = "Stop"

try {
    if ($Target.StartsWith("{")) {
        $type = [Type]::GetTypeFromCLSID([Guid]$Target)
        $omni = [Activator]::CreateInstance($type)
    } else {
        $omni = New-Object -ComObject $Target
    }
} catch {
    Write-Host "cannot create $Target : $($_.Exception.Message)"
    exit 1
}

function Show($name, $value) {
    Write-Host ("{0,-18} {1}" -f $name, $value)
}

Write-Host "object $Target"
Show "InterfaceVersion" ("{0} (0x{0:X4})" -f [int]$omni.InterfaceVersion)
Show "SoftwareVersion"  ("{0} (0x{0:X8})" -f [int]$omni.SoftwareVersion)
Show "DialogVisible"    $omni.DialogVisible
Write-Host ""

$slots = @()
foreach ($n in 1..4) {
    $rig = $null
    try { $rig = $omni."Rig$n" } catch { }
    if ($null -ne $rig) { $slots += ,@($n, $rig) }
}
Show "rigs present" $slots.Count
Write-Host ""

foreach ($pair in $slots) {
    $n = $pair[0]
    $rig = $pair[1]
    Write-Host "rig $n"
    Show "  RigType"         $rig.RigType
    Show "  Status"          ("{0} ({1})" -f [int]$rig.Status, $rig.StatusStr)
    Show "  ReadableParams"  ("0x{0:X8}" -f [int]$rig.ReadableParams)
    Show "  WriteableParams" ("0x{0:X8}" -f [int]$rig.WriteableParams)
    Show "  Freq"            $rig.Freq
    Show "  FreqA"           $rig.FreqA
    Show "  FreqB"           $rig.FreqB
    Show "  Mode"            ("0x{0:X}" -f [int]$rig.Mode)
    Show "  Vfo"             ("0x{0:X}" -f [int]$rig.Vfo)
    Show "  Split"           ("0x{0:X}" -f [int]$rig.Split)
    Show "  Rit"             ("0x{0:X}" -f [int]$rig.Rit)
    Show "  Xit"             ("0x{0:X}" -f [int]$rig.Xit)
    Show "  Tx"              ("0x{0:X}" -f [int]$rig.Tx)
    Show "  RitOffset"       $rig.RitOffset
    Show "  Pitch"           $rig.Pitch
    Show "  GetRxFrequency"  $rig.GetRxFrequency()
    Show "  GetTxFrequency"  $rig.GetTxFrequency()

    $bits = $null
    try { $bits = $rig.PortBits } catch { }
    if ($null -ne $bits) {
        Show "  PortBits.Rts" $bits.Rts
        Show "  PortBits.Dtr" $bits.Dtr
        Show "  PortBits.Cts" $bits.Cts
        Show "  PortBits.Dsr" $bits.Dsr
        [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($bits)
    } else {
        Show "  PortBits" "not available"
    }

    [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($rig)
    Write-Host ""
}

[void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($omni)