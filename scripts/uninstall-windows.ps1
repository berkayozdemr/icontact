# Remove the Windows camera part of Icontact again:
#
#     powershell -ExecutionPolicy Bypass -File scripts\uninstall-windows.ps1
#
# Removes the camera library from "C:\Program Files\Icontact", its registry
# entry, and the shared folder "C:\ProgramData\Icontact". Close Icontact and
# your meeting apps first. Asks for administrator rights once.

param([switch]$Elevated)

$ErrorActionPreference = 'Stop'
$Clsid = '{3233B581-9067-4B31-A3F2-C1C07D73BA7E}'

if (-not $Elevated) {
    $arguments = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"", '-Elevated')
    $process = Start-Process powershell -Verb RunAs -ArgumentList $arguments -Wait -PassThru
    if ($process.ExitCode -eq 0) { Write-Host 'Icontact Camera is removed.' } else { Write-Host 'Removing did not finish.' }
    exit $process.ExitCode
}

try {
    # Let go of the library first (see install-windows.ps1).
    Stop-Service -Name 'FrameServerMonitor', 'FrameServer' -Force -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue -Path "HKLM:\SOFTWARE\Classes\CLSID\$Clsid"
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue -Path (Join-Path $env:ProgramFiles 'Icontact')
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue -Path (Join-Path $env:ProgramData 'Icontact')
    exit 0
}
catch {
    Write-Host "Removing failed: $_"
    Read-Host 'Press Enter to close'
    exit 1
}
