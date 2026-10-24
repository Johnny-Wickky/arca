#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.
#
# Smoke run of the finished APK on a running emulator or device (adb): installs it, checks what
# the system recorded about it, runs it through the screen shapes of phones, tablets, foldables
# and flips (including resizing the window under a running app, which is what folding does),
# opens fixture documents, and fails on any crash, native signal or ANR. Screenshots, the view
# hierarchy of every document screen and the whole log are left in the output directory so a
# person can look at what the app actually drew.
#
#   tools/android/emulator-smoke.sh <dist-dir> <fixtures-dir> <out-dir>
#
# This proves the package loads and survives on the API level and ABI of the device it ran on
# (the CI matrix runs x86_64 images of Android 8 ... 16). It does not prove the layout is good:
# that is what the screenshots are for.
set -uo pipefail

DIST=${1:?dist directory with arca-*.apk}
FIX=${2:?fixtures directory}
OUT=${3:?output directory}
PKG=org.arca.viewer
ACT=$PKG/.MainActivity

mkdir -p "$OUT"
REPORT="$OUT/report.txt"
: > "$REPORT"
fails=0
log() { printf '%s\n' "$*" | tee -a "$REPORT"; }
fail() { log "FAIL: $*"; fails=$((fails + 1)); }
sh_() { adb shell "$@" | tr -d '\r'; }

adb wait-for-device
until [ "$(sh_ getprop sys.boot_completed)" = 1 ]; do sleep 2; done
sh_ input keyevent KEYCODE_WAKEUP >/dev/null || true
sh_ wm dismiss-keyguard >/dev/null 2>&1 || true

API=$(sh_ getprop ro.build.version.sdk)
log "device: Android $(sh_ getprop ro.build.version.release) (API $API), ABI $(sh_ getprop ro.product.cpu.abi), model $(sh_ getprop ro.product.model)"
log "display: $(sh_ wm size | tr '\n' ' ') $(sh_ wm density | tr '\n' ' ')"

APK=$(ls "$DIST"/arca-*.apk 2>/dev/null | head -n1)
[ -f "$APK" ] || { fail "no arca-*.apk in $DIST"; exit 1; }

# ---- install ---------------------------------------------------------------------------------
if adb install -r "$APK" 2>&1 | tee -a "$REPORT" | grep -q Success; then
    log "ok: installed $(basename "$APK")"
else
    fail "install"
    exit 1
fi

# What the system recorded: no permission at all, in particular not the network.
dump=$(sh_ dumpsys package $PKG)
printf '%s\n' "$dump" > "$OUT/dumpsys-package.txt"
if grep -q "android.permission.INTERNET" <<<"$dump"; then fail "INTERNET permission is present"; else log "ok: no INTERNET permission"; fi
granted=$(grep -E "^\s+android\.permission\.[A-Z_]+: granted=true" <<<"$dump" || true)
if [ -n "$granted" ]; then log "note: permissions granted to the app:"; log "$granted"; else log "ok: no permission granted"; fi
grep -E "minSdk|targetSdk|versionName|versionCode|primaryCpuAbi" <<<"$dump" | head -n 6 | tee -a "$REPORT"

shot() { adb exec-out screencap -p > "$OUT/$1.png" 2>/dev/null || log "note: no screenshot $1"; }

# Anything the system or the runtime says about our process dying.
# Only this app's own process counts: a bare "FATAL EXCEPTION" is also printed for the
# system UI or any other process of the image, which is not the app's fault (API 31 images).
crashes() {
    adb logcat -d -v brief 2>/dev/null | grep -E "ANR in $PKG|AndroidRuntime.*$PKG|Process: $PKG|Fatal signal.*$PKG|libc.*Fatal.*$PKG" || true
}
alive() { [ -n "$(sh_ pidof $PKG)" ]; }
check_state() { # check_state <label>
    local found
    found=$(crashes)
    if [ -n "$found" ]; then
        fail "$1: crash or ANR in the log"
        printf '%s\n' "$found" | head -n 20 | tee -a "$REPORT"
    elif ! alive; then
        fail "$1: the process is gone"
    else
        log "ok: $1"
    fi
}

start() { # start [extra am args...]
    adb logcat -c
    sh_ am start -W "$@" -n "$ACT" >/dev/null
}

# ---- 1. cold start, then the shapes of real devices ---------------------------------------------
# name  width x height  dpi.  The sizes are the usable display sizes of the devices named; an
# unfolded device is simply a different window size to the app, which is what is tested.
SIZES=(
    "small-phone-720x1280 720x1280 320"           # 4.7-5" phones, Android 8 era
    "phone-1080x2400 1080x2400 420"               # typical phone
    "tall-phone-1440x3200 1440x3200 560"          # 6.8" flagship
    "fold-cover-904x2316 904x2316 420"            # Galaxy Z Fold cover display, narrow
    "fold-inner-1812x2176 1812x2176 420"          # Galaxy Z Fold inner display, almost square
    "pixel-fold-inner-2208x1840 2208x1840 420"    # Pixel Fold inner display
    "flip-main-1080x2640 1080x2640 425"           # Galaxy Z Flip main display
    "flip-cover-720x748 720x748 300"              # Flip cover display, nearly square and tiny
    "tecno-flip-1080x2640 1080x2640 440"          # Tecno Phantom V Flip class
    "tablet-portrait-1600x2560 1600x2560 320"     # 10.9" tablet
    "tablet-landscape-2560x1600 2560x1600 320"
    "tablet-small-800x1280 800x1280 213"          # 8" budget tablet
    "split-screen-1080x1200 1080x1200 420"        # half of a phone in split-screen
    "freeform-window-900x700 900x700 240"         # Chromebook / desktop-mode window
)

log ""
log "== cold start"
start
sleep 8
check_state "cold start"
shot "00-cold-start"

log ""
log "== resizing the window under the running app (folding, unfolding, split-screen)"
for entry in "${SIZES[@]}"; do
    read -r name size dpi <<<"$entry"
    sh_ wm size "$size" >/dev/null
    sh_ wm density "$dpi" >/dev/null
    sleep 3
    check_state "resized to $name ($size @ ${dpi}dpi)"
    shot "10-resize-$name"
done
# Rotation at the end of the list's last size, then back.
sh_ settings put system accelerometer_rotation 0 >/dev/null
sh_ settings put system user_rotation 1 >/dev/null
sleep 3
check_state "rotated to landscape"
shot "11-rotated-landscape"
sh_ settings put system user_rotation 0 >/dev/null
sleep 3
check_state "rotated back to portrait"
sh_ wm size reset >/dev/null
sh_ wm density reset >/dev/null

# ---- 2. text size and dark theme ------------------------------------------------------------------
log ""
log "== large text and dark theme"
sh_ settings put system font_scale 1.3 >/dev/null
sleep 3
check_state "font scale 1.3"
shot "20-font-1.3"
sh_ settings put system font_scale 2.0 >/dev/null 2>&1 || true
sleep 3
check_state "font scale 2.0"
shot "21-font-2.0"
sh_ settings put system font_scale 1.0 >/dev/null
if [ "$API" -ge 29 ]; then
    sh_ cmd uimode night yes >/dev/null 2>&1 || true
    sleep 4
    check_state "dark theme"
    shot "22-dark"
    sh_ cmd uimode night no >/dev/null 2>&1 || true
    sleep 3
fi

# ---- 3. documents through the system's "open with" ---------------------------------------------
log ""
log "== documents (VIEW intent with a content:// address, no storage permission for the app)"
mime_of() {
    case "$1" in
        *.docx) echo application/vnd.openxmlformats-officedocument.wordprocessingml.document ;;
        *.xlsx) echo application/vnd.openxmlformats-officedocument.spreadsheetml.sheet ;;
        *.pptx) echo application/vnd.openxmlformats-officedocument.presentationml.presentation ;;
        *.doc) echo application/msword ;;
        *.xls) echo application/vnd.ms-excel ;;
        *.ppt) echo application/vnd.ms-powerpoint ;;
        *.odt) echo application/vnd.oasis.opendocument.text ;;
        *.ods) echo application/vnd.oasis.opendocument.spreadsheet ;;
        *.odp) echo application/vnd.oasis.opendocument.presentation ;;
        *.pdf) echo application/pdf ;;
        *.rtf) echo application/rtf ;;
        *.csv) echo text/csv ;;
        *) echo application/octet-stream ;;
    esac
}

open_doc() { # open_doc <file> <label>
    local f=$1 label=$2 id uri
    adb push "$FIX/$f" "/sdcard/Download/$f" >/dev/null 2>&1 || { log "note: cannot push $f"; return 1; }
    # The whole `content` command goes to the device shell as ONE string: with separate arguments
    # adb joins them and the shell strips the quotes of the --where clause ("Invalid token").
    if [ "$API" -ge 29 ]; then
        # A row of the Downloads collection that the app can be handed (shell may write it).
        adb shell "content insert --uri content://media/external/downloads --bind _display_name:s:$f --bind mime_type:s:$(mime_of "$f") --bind relative_path:s:Download/Arca-smoke" >/dev/null 2>&1 || true
        id=$(adb shell "content query --uri content://media/external/downloads --projection _id --where \"_display_name='$f'\"" 2>/dev/null \
            | tr -d '\r' | sed -n 's/.*_id=\([0-9][0-9]*\).*/\1/p' | tail -n1)
        if [ -n "$id" ]; then
            # Where the row says its file is.
            data=$(adb shell "content query --uri content://media/external/downloads/$id --projection _data" 2>/dev/null \
                | tr -d '\r' | sed -n 's/.*_data=\(.*\)$/\1/p' | head -n1)
            adb shell "cat /sdcard/Download/$f | content write --uri content://media/external/downloads/$id" >/dev/null 2>&1 || true
            # `content write` exits 0 even when the provider refused it (API 30: "Package ...
            # does not belong to 2000"), and a row that was just inserted has no file yet. So look
            # at the file itself, and if it is not there put it where the row says.
            if [ -n "$data" ] && ! adb shell "[ -s '$data' ]"; then
                adb shell "mkdir -p '$(dirname "$data")'" >/dev/null 2>&1 || true
                adb push "$FIX/$f" "$data" >/dev/null 2>&1 || true
            fi
            # (If the row has no readable path there is nothing to check: trust the write, as before.)
            if [ -z "$data" ] || adb shell "[ -s '$data' ]"; then
                uri="content://media/external/downloads/$id"
            fi
        fi
    fi
    if [ -z "${uri:-}" ]; then
        adb shell "am broadcast -a android.intent.action.MEDIA_SCANNER_SCAN_FILE -d file:///sdcard/Download/$f" >/dev/null 2>&1 || true
        sleep 1
        id=$(adb shell "content query --uri content://media/external/file --projection _id --where \"_display_name='$f'\"" 2>/dev/null \
            | tr -d '\r' | sed -n 's/.*_id=\([0-9][0-9]*\).*/\1/p' | head -n1)
        [ -n "$id" ] && uri="content://media/external/file/$id"
    fi
    if [ -z "${uri:-}" ]; then
        log "skip: $f cannot be exposed through MediaStore on API $API"
        return 1
    fi
    sh_ am force-stop $PKG >/dev/null
    start --grant-read-uri-permission -a android.intent.action.VIEW \
        -d "$uri" -t "$(mime_of "$f")"
    sleep 10
    check_state "$label: $f"
    shot "30-$label-${f//./_}"
    sh_ uiautomator dump /sdcard/arca-ui.xml >/dev/null 2>&1 || true
    adb pull /sdcard/arca-ui.xml "$OUT/ui-$label-${f//./_}.xml" >/dev/null 2>&1 || true
    # The app must have read the document, not just stayed alive on an error screen.
    if grep -q "Could not open the file" "$OUT/ui-$label-${f//./_}.xml" 2>/dev/null; then
        fail "$label: $f: the app could not open the document it was handed"
    elif grep -q "Threat X-Ray" "$OUT/ui-$label-${f//./_}.xml" 2>/dev/null; then
        log "ok: $label: $f was scanned"
    else
        log "note: $label: $f: no result screen found in the view hierarchy"
    fi
    return 0
}

DOCS=(report.docx report.doc report.odt report.rtf report.pdf report.txt sheet.xlsx sheet.xls sheet.ods sheet.csv
      slides.pptx slides.ppt slides.odp deck-rich.pptx deck-rich.ppt lo-rich.doc)
for entry in "phone-1080x2400 1080x2400 420" "fold-inner-1812x2176 1812x2176 420" "tablet-landscape-2560x1600 2560x1600 320"; do
    read -r name size dpi <<<"$entry"
    sh_ wm size "$size" >/dev/null
    sh_ wm density "$dpi" >/dev/null
    sleep 2
    for f in "${DOCS[@]}"; do
        [ -f "$FIX/$f" ] || continue
        open_doc "$f" "$name" || break
    done
done
sh_ wm size reset >/dev/null
sh_ wm density reset >/dev/null

# ---- 4. the whole log, for a human --------------------------------------------------------------
adb logcat -d -v threadtime > "$OUT/logcat.txt" 2>/dev/null || true
grep -E "arca|Arca" "$OUT/logcat.txt" > "$OUT/logcat-arca.txt" 2>/dev/null || true

log ""
if [ "$fails" = 0 ]; then
    log "RESULT: no crash, signal or ANR on API $API"
    exit 0
fi
log "RESULT: $fails failure(s) on API $API"
exit 1
