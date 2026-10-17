#!/usr/bin/env bash
# Probes the KWin interfaces barux depends on, so docs/kwin-notes.md can be
# filled in from what is actually true on a machine instead of what the plan
# assumed. Run it as your SESSION USER (not root): it needs your session bus and
# runtime directory, and its install stage writes under your ~/.local/share and
# your kwinrc.
#
# Usage: tools/kwin-probe.sh [stage ...]
#   stages: introspect  the virtual-desktop and KWin interfaces over D-Bus
#           monitor     watch org.kde.KWin for one desktop switch you make
#           journal     the running KWin script's console output
#           install     install+enable dist/kwin-script and reconfigure KWin
#           uninstall   reverse the install stage
#           all         introspect, monitor, install, journal, uninstall (default)
# Each stage is independent; run them one at a time to read the output between.
# Nothing here needs root, and nothing outside the script install is written.

set -uo pipefail
cd "$(dirname "$0")/.."

SCRIPT_ID=org.barux.desktop
SCRIPT_SRC=dist/kwin-script
SCRIPT_DEST="${XDG_DATA_HOME:-$HOME/.local/share}/kwin/scripts/$SCRIPT_ID"
KWINRC="${XDG_CONFIG_HOME:-$HOME/.config}/kwinrc"

have() { command -v "$1" >/dev/null 2>&1; }
say() { printf '\n==> %s\n' "$1"; }

# The session bus, as the session user (XDG_RUNTIME_DIR etc. should already be set).
qdbus() {
    if have qdbus6; then qdbus6 "$@"; else qdbus "$@"; fi
}

# The desktop interface: barux reads current, desktops and their names here.
# Task 3 is the task that consumes this.
stage_introspect() {
    say "org.kde.KWin /VirtualDesktopManager (properties and signals)"
    if have qdbus6 || have qdbus; then
        qdbus org.kde.KWin /VirtualDesktopManager 2>&1 | sed 's/^/  /'
        echo "  -- properties --"
        qdbus org.kde.KWin /VirtualDesktopManager org.freedesktop.DBus.Properties \
            GetAll org.kde.KWin.VirtualDesktopManager 2>&1 | sed 's/^/  /'
    else
        echo "  qdbus6/qdbus not found; install qt6-tools or use gdbus:"
        echo "  gdbus call --session --dest org.kde.KWin --object-path /VirtualDesktopManager \\"
        echo "      --method org.freedesktop.DBus.Properties.GetAll org.kde.KWin.VirtualDesktopManager"
    fi
    if have gdbus; then
        say "gdbus introspection of /VirtualDesktopManager"
        gdbus introspect --session --dest org.kde.KWin --object-path /VirtualDesktopManager 2>&1 | sed 's/^/  /'
    fi
    say "/KWin (scripting and reconfigure live here)"
    if have gdbus; then
        gdbus introspect --session --dest org.kde.KWin --object-path /KWin 2>&1 | sed 's/^/  /'
    fi
}

# A desktop switch must arrive as a signal, not from a timer (Task 3).
stage_monitor() {
    say "watching org.kde.KWin for a desktop switch — switch desktops now (Ctrl+F1..)"
    local secs=${MONITOR_SECONDS:-10}
    timeout "$secs" dbus-monitor --session \
        "type='signal',sender='org.kde.KWin'" 2>&1 | sed 's/^/  /'
    echo "  (watched ${secs}s; set MONITOR_SECONDS to watch longer)"
}

# The installed script's console.log goes to the launcher's journal.
stage_journal() {
    say "recent KWin script output (needs the systemd-journal/wheel group)"
    journalctl -b --user-unit plasma-kwin_wayland.service --no-pager 2>/dev/null |
        grep -E "barux snapshot|$SCRIPT_ID" | tail -n 20 | sed 's/^/  /' ||
        echo "  no matching lines (script not installed/enabled, or no journal access)"
}

stage_install() {
    say "installing $SCRIPT_SRC -> $SCRIPT_DEST and enabling it in kwinrc"
    mkdir -p "$SCRIPT_DEST"
    cp -R "$SCRIPT_SRC/." "$SCRIPT_DEST/"
    echo "  copied; metadata id: $(grep -o '"Id"[^,]*' "$SCRIPT_DEST/metadata.json" 2>/dev/null)"
    if have kwriteconfig6; then
        kwriteconfig6 --file kwinrc --group Plugins --key "${SCRIPT_ID}Enabled" true
        echo "  kwriteconfig6: ${SCRIPT_ID}Enabled=true"
    else
        # No kwriteconfig6: append the group+key. Check the file first — editing
        # kwinrc by hand is exactly what this exists to avoid.
        {
            printf '\n[Plugins]\n%sEnabled=true\n' "$SCRIPT_ID"
        } >>"$KWINRC"
        echo "  appended [Plugins] ${SCRIPT_ID}Enabled=true to $KWINRC"
    fi
    if have qdbus6 || have qdbus; then
        qdbus org.kde.KWin /KWin reconfigure 2>&1 | sed 's/^/  reconfigure: /'
    else
        echo "  run: qdbus6 org.kde.KWin /KWin reconfigure   (to make KWin load it now)"
    fi
    echo "  then: tools/kwin-probe.sh journal   (or the KWin debug console)"
}

stage_uninstall() {
    say "removing the script and disabling it in kwinrc"
    rm -rf "$SCRIPT_DEST" && echo "  removed $SCRIPT_DEST"
    if have kwriteconfig6; then
        kwriteconfig6 --file kwinrc --group Plugins --key "${SCRIPT_ID}Enabled" --delete
        echo "  kwriteconfig6: deleted ${SCRIPT_ID}Enabled"
    else
        echo "  remove the ${SCRIPT_ID}Enabled line from [Plugins] in $KWINRC"
    fi
    if have qdbus6 || have qdbus; then
        qdbus org.kde.KWin /KWin reconfigure 2>&1 | sed 's/^/  reconfigure: /'
    fi
    if grep -q "$SCRIPT_ID" "$KWINRC" 2>/dev/null; then
        echo "  NOTE: $KWINRC still mentions $SCRIPT_ID — remove it by hand"
    fi
}

stages=("$@")
((${#stages[@]})) || stages=(all)
for stage in "${stages[@]}"; do
    case $stage in
        introspect) stage_introspect ;;
        monitor) stage_monitor ;;
        journal) stage_journal ;;
        install) stage_install ;;
        uninstall) stage_uninstall ;;
        all)
            stage_introspect
            stage_install
            stage_journal
            stage_monitor
            stage_uninstall
            ;;
        -h | --help) sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown stage $stage (see --help)" >&2; exit 2 ;;
    esac
done
