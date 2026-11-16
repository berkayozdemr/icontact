# Install the Windows 11 camera part of Icontact. Run once, from the project folder:
#
#     powershell -ExecutionPolicy Bypass -File scripts\install-windows.ps1
#
# What it does:
#   1. Builds the app and the camera library (needs Rust, https://rustup.rs).
#   2. Asks for administrator rights, once, and then:
#      - copies icontact_mfcam.dll to "C:\Program Files\Icontact"
#        (a folder only administrators can change, because Windows' own
#        camera service loads the file),
#      - tells Windows where that library is (one registry entry),
#      - makes the folder "C:\ProgramData\Icontact", where the app leaves its
#        finished pictures for the camera and the camera keeps a small log.
#
# After that, start Icontact (target\release\ec-app.exe) and pick
# "Icontact Camera" in your meeting app. Needs Windows 11.
#
# Undo everything with scripts\uninstall-windows.ps1.
#
# NOT YET RUN ON A REAL MACHINE: written on a Mac. If a step fails, the
# message says which one.

param(
    # Set by the script itself when it starts its administrator half.
    [switch]$Elevated,
    [string]$Dll
)

$ErrorActionPreference = 'Stop'

# The camera's class id. Must match WIN_CAMERA_CLSID in crates\ec-sink\src\lib.rs.
$Clsid = '{3233B581-9067-4B31-A3F2-C1C07D73BA7E}'
$InstallDir = Join-Path $env:ProgramFiles 'Icontact'
$DataDir = Join-Path $env:ProgramData 'Icontact'

if (-not $Elevated) {
    # ---- ordinary half: build ----------------------------------------------
    Set-Location (Join-Path $PSScriptRoot '..')

    if ([Environment]::OSVersion.Version.Build -lt 22000) {
        Write-Host 'This camera needs Windows 11. On Windows 10 the app still runs, but meeting apps cannot see it yet.'
        exit 1
    }
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Write-Host 'Rust is not installed. Get it from https://rustup.rs and run this again.'
        exit 1
    }

    Write-Host 'Building the app...'
    cargo build --release -p ec-app
    if ($LASTEXITCODE -ne 0) { Write-Host 'Building the app failed.'; exit 1 }

    # The camera library is built so that it carries Microsoft's C runtime
    # inside itself ("crt-static"). It is loaded by a Windows service, and we
    # cannot count on that runtime being installed where the service looks.
    # A separate output folder keeps this setting away from the app's build.
    Write-Host 'Building the camera library...'
    $env:RUSTFLAGS = '-C target-feature=+crt-static'
    cargo build --release -p ec-mfcam --target-dir target\mfcam
    $built = $LASTEXITCODE
    Remove-Item Env:RUSTFLAGS
    if ($built -ne 0) { Write-Host 'Building the camera library failed.'; exit 1 }

    $dllPath = (Resolve-Path 'target\mfcam\release\icontact_mfcam.dll').Path

    Write-Host 'Asking for administrator rights to install the camera library...'
    $arguments = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"", '-Elevated', '-Dll', "`"$dllPath`"")
    $process = Start-Process powershell -Verb RunAs -ArgumentList $arguments -Wait -PassThru
    if ($process.ExitCode -ne 0) {
        Write-Host 'The install step did not finish. Nothing else was changed.'
        exit 1
    }
    Write-Host ''
    Write-Host 'Done. Start Icontact with:  target\release\ec-app.exe'
    Write-Host 'Then pick "Icontact Camera" in your meeting app.'
    exit 0
}

# ---- administrator half: copy and register ----------------------------------
try {
    # Windows' camera service may have an older copy of the library open.
    # Stopping it lets the file be replaced; Windows starts it again by itself
    # the next time any program uses a camera.
    Stop-Service -Name 'FrameServerMonitor', 'FrameServer' -Force -ErrorAction SilentlyContinue

    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    Copy-Item -Force -Path $Dll -Destination (Join-Path $InstallDir 'icontact_mfcam.dll')

    # The registry entry: "the class with this id lives in this DLL".
    $key = "HKLM:\SOFTWARE\Classes\CLSID\$Clsid"
    New-Item -Force -Path $key | Out-Null
    Set-Item -Path $key -Value 'Icontact Camera'
    New-Item -Force -Path "$key\InprocServer32" | Out-Null
    Set-Item -Path "$key\InprocServer32" -Value (Join-Path $InstallDir 'icontact_mfcam.dll')
    Set-ItemProperty -Path "$key\InprocServer32" -Name 'ThreadingModel' -Value 'Both'

    # The shared folder. Ordinary users (the app) and the account the camera
    # service runs as ("Local Service", S-1-5-19) may both change files in it.
    New-Item -ItemType Directory -Force -Path $DataDir | Out-Null
    icacls $DataDir /grant '*S-1-5-32-545:(OI)(CI)M' '*S-1-5-19:(OI)(CI)M' | Out-Null

    exit 0
}
catch {
    Write-Host "Install failed: $_"
    Read-Host 'Press Enter to close'
    exit 1
}
