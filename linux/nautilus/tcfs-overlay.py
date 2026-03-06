"""TCFS Nautilus extension -- overlay icons and context menu for sync status.

Install:
    cp tcfs-overlay.py ~/.local/share/nautilus-python/extensions/

Requires:
    - nautilus-python (python3-nautilus)
    - D-Bus session bus with io.tinyland.tcfs service running
"""

import gi

gi.require_version("Nautilus", "4.0")
from gi.repository import Nautilus, GObject, Gio, GLib  # noqa: E402

# D-Bus constants
DBUS_NAME = "io.tinyland.tcfs"
DBUS_PATH = "/io/tinyland/tcfs"
DBUS_IFACE = "io.tinyland.tcfs"

# Map sync status string -> Nautilus emblem name
_STATUS_EMBLEMS = {
    "synced": "emblem-default",
    "syncing": "emblem-synchronizing",
    "placeholder": "emblem-downloads",
    "conflict": "emblem-important",
    "error": "emblem-unreadable",
    # "unknown" -> no emblem
}


class TcfsOverlayExtension(
    GObject.GObject,
    Nautilus.InfoProvider,
    Nautilus.MenuProvider,
):
    """Nautilus extension providing TCFS sync status overlays and actions."""

    def __init__(self):
        super().__init__()
        self._proxy = None
        self._connect_dbus()

    # ------------------------------------------------------------------
    # D-Bus helpers
    # ------------------------------------------------------------------

    def _connect_dbus(self):
        """Connect to the TCFS D-Bus service (best-effort)."""
        try:
            bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
            self._proxy = Gio.DBusProxy.new_sync(
                bus,
                Gio.DBusProxyFlags.DO_NOT_AUTO_START,
                None,
                DBUS_NAME,
                DBUS_PATH,
                DBUS_IFACE,
                None,
            )
            # Subscribe to StatusChanged signal
            self._proxy.connect("g-signal", self._on_signal)
        except Exception:
            self._proxy = None

    def _get_status(self, path):
        """Query sync status for a single path. Returns status string."""
        if self._proxy is None:
            self._connect_dbus()
        if self._proxy is None:
            return "unknown"
        try:
            result = self._proxy.call_sync(
                "GetStatus",
                GLib.Variant("(s)", (path,)),
                Gio.DBusCallFlags.NONE,
                500,  # timeout ms
                None,
            )
            return result.unpack()[0] if result else "unknown"
        except Exception:
            return "unknown"

    def _call_action(self, method, path):
        """Call a D-Bus method (Sync / Unsync) for a path."""
        if self._proxy is None:
            return
        try:
            self._proxy.call_sync(
                method,
                GLib.Variant("(s)", (path,)),
                Gio.DBusCallFlags.NONE,
                5000,
                None,
            )
        except Exception:
            pass

    def _on_signal(self, _proxy, _sender, signal_name, parameters):
        """Handle StatusChanged signal (future: invalidate file info)."""
        if signal_name == "StatusChanged":
            _path, _status = parameters.unpack()
            # Nautilus 4 doesn't expose a public invalidation API from
            # Python, so status will refresh on next directory load.

    # ------------------------------------------------------------------
    # Nautilus.InfoProvider
    # ------------------------------------------------------------------

    def update_file_info(self, file):  # noqa: A003 (Nautilus API name)
        """Called by Nautilus to add emblem overlays to each file."""
        if file.get_uri_scheme() != "file":
            return

        path = file.get_location().get_path()
        if path is None:
            return

        status = self._get_status(path)
        emblem = _STATUS_EMBLEMS.get(status)
        if emblem:
            file.add_emblem(emblem)

    # ------------------------------------------------------------------
    # Nautilus.MenuProvider
    # ------------------------------------------------------------------

    def get_file_items(self, files):
        """Add context-menu items for selected files."""
        if not files:
            return []

        items = []

        # "Sync Now" item
        sync_item = Nautilus.MenuItem(
            name="TcfsOverlay::SyncNow",
            label="TCFS: Sync Now",
            tip="Download this file/folder from the TCFS cloud",
        )
        sync_item.connect("activate", self._on_sync_now, files)
        items.append(sync_item)

        # "Remove Local Copy" item
        unsync_item = Nautilus.MenuItem(
            name="TcfsOverlay::RemoveLocal",
            label="TCFS: Remove Local Copy",
            tip="Dehydrate this file (keep cloud copy only)",
        )
        unsync_item.connect("activate", self._on_unsync, files)
        items.append(unsync_item)

        # Show "Resolve Conflict" only when at least one file is in conflict
        paths = [f.get_location().get_path() for f in files if f.get_location()]
        if any(self._get_status(p) == "conflict" for p in paths if p):
            conflict_item = Nautilus.MenuItem(
                name="TcfsOverlay::ResolveConflict",
                label="TCFS: Resolve Conflict",
                tip="Open the conflict resolution dialog",
            )
            conflict_item.connect("activate", self._on_resolve_conflict, files)
            items.append(conflict_item)

        return items

    # ------------------------------------------------------------------
    # Menu action callbacks
    # ------------------------------------------------------------------

    def _on_sync_now(self, _menu_item, files):
        for f in files:
            loc = f.get_location()
            if loc:
                path = loc.get_path()
                if path:
                    self._call_action("Sync", path)

    def _on_unsync(self, _menu_item, files):
        for f in files:
            loc = f.get_location()
            if loc:
                path = loc.get_path()
                if path:
                    self._call_action("Unsync", path)

    def _on_resolve_conflict(self, _menu_item, files):
        """Open conflict resolution in a terminal."""
        import subprocess

        for f in files:
            loc = f.get_location()
            if loc:
                path = loc.get_path()
                if path and self._get_status(path) == "conflict":
                    # Launch tcfs resolve in a terminal for interactive resolution
                    try:
                        subprocess.Popen(
                            ["tcfs", "resolve", path],
                            start_new_session=True,
                        )
                    except FileNotFoundError:
                        # tcfs CLI not in PATH — try via D-Bus fallback
                        self._call_action("Sync", path)
