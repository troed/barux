// barux desktop bridge (KWin script).
//
// KWin exposes no window list over D-Bus by itself, so barux ships this script:
// KWin loads it, and on every change it pushes a JSON snapshot of the virtual
// desktops and the window list to the name barux registers on the session bus
// (org.barux.KWin, object /KWin, method Snapshot). install.sh installs and enables
// it (see Task 10); docs/kwin-notes.md records what is verified on the machine.
//
// Snapshot shape (the wire contract; see src/kwin.rs's Snapshot struct):
//   {
//     "desktops": [ { "position": <int, 1-based>, "name": <string> } ],
//     "current":  <int, the current desktop position>,
//     "windows":  [ { "id": <string>, "class": <string>,
//                     "title": <string>, "desktop": <int> } ],
//     "focused":  <string id> | null
//   }

var BARUX_SERVICE = "org.barux.KWin";
var BARUX_PATH = "/KWin";
var BARUX_IFACE = "org.barux.KWin";

// The virtual desktops, by position (1-based) and name.
function desktopList() {
    var out = [];
    var desktops = workspace.desktops;
    for (var i = 0; i < desktops.length; i++) {
        out.push({ position: desktops[i].x11DesktopNumber, name: desktops[i].name });
    }
    return out;
}

// The desktop a window is filed under: the first of its desktops, else 0.
function windowDesktop(win) {
    var desktops = win.desktops || [];
    return desktops.length > 0 ? desktops[0].x11DesktopNumber : 0;
}

// The class barux matches on: KWin's resourceClass, with resourceName and the
// desktop file as fallbacks for windows that carry none, lower-cased so it lines
// up with Hyprland-style config values (design §6.2).
function windowClass(win) {
    var cls = win.resourceClass || win.resourceName || win.desktopFileName || "";
    return String(cls).toLowerCase();
}

function currentPosition() {
    return workspace.currentDesktop ? workspace.currentDesktop.x11DesktopNumber : 0;
}

function focusedId() {
    var active = workspace.activeWindow;
    return active ? String(active.internalId) : null;
}

// One full snapshot: every window with the fields barux's strip needs, the desktop
// list, and the focused window's id.
function snapshot() {
    var windows = [];
    var list = workspace.windowList();
    for (var i = 0; i < list.length; i++) {
        var w = list[i];
        windows.push({
            id: String(w.internalId),
            class: windowClass(w),
            title: w.caption || "",
            desktop: windowDesktop(w),
        });
    }
    return {
        desktops: desktopList(),
        current: currentPosition(),
        windows: windows,
        focused: focusedId(),
    };
}

function send() {
    var json = JSON.stringify(snapshot());
    callDBus(BARUX_SERVICE, BARUX_PATH, BARUX_IFACE, "Snapshot", json);
}

// Per-window desktopsChanged is not a workspace signal: wire it on the window, and
// unwire it when the window goes away so the closures do not accumulate.
function watch(win) {
    win.desktopsChanged.connect(send);
}

workspace.windowAdded.connect(function (win) {
    watch(win);
    send();
});
workspace.windowRemoved.connect(function (win) {
    try {
        win.desktopsChanged.disconnect(send);
    } catch (e) {
        // Already gone; nothing to unwire.
    }
    send();
});
workspace.windowActivated.connect(send);
workspace.currentDesktopChanged.connect(send);

// Windows already present when the script loads.
var existing = workspace.windowList();
for (var i = 0; i < existing.length; i++) {
    watch(existing[i]);
}

send();
