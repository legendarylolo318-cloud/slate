#!/bin/sh
# Sourced by AppRun (linuxdeploy apprun-hooks). Keep the plugin registry in a
# user-writable place; the AppImage mount is read-only.
export GST_REGISTRY_1_0="${XDG_CACHE_HOME:-$HOME/.cache}/slate/gst-registry.bin"
mkdir -p "$(dirname "$GST_REGISTRY_1_0")" 2>/dev/null || true
export GST_PLUGIN_SYSTEM_PATH_1_0="$APPDIR/usr/lib/gstreamer-1.0"
export GST_PLUGIN_PATH_1_0="$APPDIR/usr/lib/gstreamer-1.0"
export GST_PLUGIN_SCANNER_1_0="$APPDIR/usr/lib/gstreamer1.0/gstreamer-1.0/gst-plugin-scanner"
# GIO's TLS backend is bundled (the host's may be ABI-incompatible or at a
# different path); souphttpsrc needs it for HTTPS.
export GIO_MODULE_DIR="$APPDIR/usr/lib/gio/modules"
