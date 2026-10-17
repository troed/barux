// barux desktop bridge (KWin script).
//
// KWin exposes no window list over D-Bus by itself, so barux ships this script:
// KWin loads it, and on every change it pushes a JSON snapshot of the virtual
// desktops and the window list to the name barux registers on the session bus
// (org.barux.KWin). This file is installed by install.sh (see Task 10); it is the
// probe shape from Task 1 — Task 4 narrows it to the final wire shape.

var BARUX_SERVICE = "org.barux.KWin";
var BARUX_PATH = "/probe";
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

// The positions of the desktops a window is on (Plasma allows more than one).
function windowDesktops(win) {
    var out = [];
    var desktops = win.desktops || [];
    for (var i = 0; i < desktops.length; i++) {
        out.push(desktops[i].x11DesktopNumber);
    }
    return out;
}

function currentPosition() {
    return workspace.currentDesktop ? workspace.currentDesktop.x11DesktopNumber : 0;
}

function focusedId() {
    var active = workspace.activeWindow;
    return active ? String(active.internalId) : null;
}

// One full snapshot: every window with the fields barux's strip needs, plus the
// desktop list and the focused window's id.
function snapshot() {
    var windows = [];
    var list = workspace.windowList();
    for (var i = 0; i < list.length; i++) {
        var w = list[i];
        windows.push({
            internalId: String(w.internalId),
            resourceClass: w.resourceClass || "",
            resourceName: w.resourceName || "",
            desktopFileName: w.desktopFileName || "",
            caption: w.caption || "",
            desktops: windowDesktops(w),
            active: !!w.active,
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
    console.log("barux snapshot " + json);
    callDBus(BARUX_SERVICE, BARUX_PATH, BARUX_IFACE, "Snapshot", json);
}

// Per-window desktopsChanged is not a workspace signal: wire it on the window,
// and unwire it when the window goes away so the closures do not accumulate.
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
