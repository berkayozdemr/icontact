#!/bin/sh
# Everything a Mac needs, in one command, after cloning the repository:
#
#     scripts/setup-mac.sh
#
# What it does:
#   1. Builds Icontact.app (needs Rust; see README).
#   2. Makes sure the camera that meeting apps will pick exists. Icontact
#      borrows the one that the free program OBS installs, because macOS only
#      lets programs signed with a paid Apple account add a camera of their
#      own. OBS is needed once, to install that camera. It never has to be
#      open afterwards.
#   3. Starts Icontact.
#
# One step cannot be done by any script: macOS wants a person to flip the
# switch that allows the camera. The script opens the right settings page and
# waits for you.
#
# Running it again is harmless: finished steps are skipped.

set -eu

cd "$(dirname "$0")/.."
export DEVELOPER_DIR="${DEVELOPER_DIR:-/Library/Developer/CommandLineTools}"

if ! command -v cargo >/dev/null 2>&1; then
    echo "Rust is not installed. Get it from https://rustup.rs and run this again."
    exit 1
fi

# --- 1. build -----------------------------------------------------------------
scripts/make-mac-app.sh
# ec-cli holds the camera check used below.
cargo build --release -p ec-cli
check="target/release/ec-cli obs-check"

# Is the camera there? (The check ends with a failure code while it is not.)
camera_ready() {
    $check >/dev/null 2>&1
}

obs_installed() {
    [ -d /Applications/OBS.app ] || [ -d "$HOME/Applications/OBS.app" ]
}

# --- 2. the camera --------------------------------------------------------------
if camera_ready; then
    echo "The camera is already set up."
else
    if ! obs_installed; then
        echo
        echo "Icontact needs the free program OBS once, for the camera it installs."
        if command -v brew >/dev/null 2>&1; then
            printf "Install OBS now with Homebrew? [y/N] "
            read -r answer
            case "$answer" in
                y | Y | yes) brew install --cask obs ;;
            esac
        fi
        if ! obs_installed; then
            echo "Install OBS 30 or newer from https://obsproject.com (move it to Applications),"
            echo "then run this script again."
            exit 1
        fi
    fi

    echo
    echo "Starting OBS with its virtual camera, which makes macOS install the camera..."
    # --startvirtualcam is OBS's own option: the same as pressing
    # "Start Virtual Camera" by hand.
    open -a OBS --args --startvirtualcam
    sleep 8

    if ! camera_ready; then
        echo
        echo "macOS now wants your permission. In the window that opens:"
        echo "    Camera Extensions > switch OBS on"
        echo "(If OBS shows questions of its own first, answer them; if nothing"
        echo " happens, press \"Start Virtual Camera\" in OBS, bottom right.)"
        # Opens System Settings > General > Login Items & Extensions.
        open "x-apple.systempreferences:com.apple.LoginItems-Settings.extension" || true
        printf "Waiting for the camera"
        tries=0
        until camera_ready; do
            tries=$((tries + 1))
            if [ "$tries" -gt 150 ]; then # 5 minutes
                echo
                echo "The camera did not appear. What is still missing:"
                $check || true
                exit 1
            fi
            printf "."
            sleep 2
        done
        echo
    fi
    echo "The camera is set up. OBS is not needed any more; closing it."
    osascript -e 'tell application "OBS" to quit' >/dev/null 2>&1 || true
fi

# --- 3. run ---------------------------------------------------------------------
echo
echo "Starting Icontact. In your meeting app, pick the camera \"OBS Virtual Camera\"."
open target/Icontact.app
