Option Explicit

Dim omni, rig, waited

Set omni = CreateObject("Detent.OmniRigX")

WScript.Echo "InterfaceVersion 0x" & Hex(omni.InterfaceVersion)
WScript.Echo "SoftwareVersion  0x" & Hex(omni.SoftwareVersion)
WScript.Echo ""

' fucking hell dude
Set rig = omni.Rig1
waited = 0
Do While rig.Status <> 4 And waited < 5000
    WScript.Sleep 100
    waited = waited + 100
Loop
WScript.Echo "waited " & waited & " ms"
WScript.Echo ""

ShowRig 1, rig
ShowRig 2, omni.Rig2

Sub ShowRig(n, r)
    WScript.Echo "rig " & n
    WScript.Echo "  RigType         " & r.RigType
    WScript.Echo "  Status          " & r.Status & " (" & r.StatusStr & ")"
    WScript.Echo "  ReadableParams  0x" & Hex(r.ReadableParams)
    WScript.Echo "  WriteableParams 0x" & Hex(r.WriteableParams)
    WScript.Echo "  Freq            " & r.Freq
    WScript.Echo "  Mode            0x" & Hex(r.Mode)
    WScript.Echo "  Tx              0x" & Hex(r.Tx)
    WScript.Echo "  GetRxFrequency  " & r.GetRxFrequency()
    WScript.Echo "  GetTxFrequency  " & r.GetTxFrequency()
    WScript.Echo ""
End Sub