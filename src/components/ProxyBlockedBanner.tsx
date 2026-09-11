import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { safeErrorMessage } from "../lib/errorUtils";
import type { ProxySettings } from "../atoms/proxy";

/** The serialized `proxy::ProxyStatus`, `#[serde(tag = "state")]`. */
export type ProxyStatus =
  | { state: "off" }
  | { state: "active"; degraded: string[] }
  | { state: "blocked"; reason: string };

/** Dispatched on `window` whenever a save may have changed the block. The
 *  banner re-reads the status rather than guessing from the settings it sent:
 *  the backend, not the form, decides whether a proxy is usable. */
export const PROXY_STATUS_EVENT = "sone:proxy-status";

/** The reason to show, or null when there is nothing to report.
 *
 *  Only `blocked` renders. `degraded` is a per-feature notice and is empty
 *  until the host-capability probe exists; `off` and `active` are the normal
 *  states and must not put a bar across the window. */
export function blockedReason(status: unknown): string | null {
  if (typeof status !== "object" || status === null) return null;
  const s = status as { state?: unknown; reason?: unknown };
  if (s.state !== "blocked") return null;
  if (typeof s.reason !== "string") return "The proxy settings are unusable";
  const reason = s.reason.trim();
  return reason.length > 0 ? reason : "The proxy settings are unusable";
}

/**
 * A block means every request is refused, including the login request — so
 * this bar has to render outside the authenticated shell.
 *
 * The trap it exists to close: a proxy that persists but cannot build (a
 * SOCKS5 host that does not resolve is the reachable case — it plans fine and
 * fails while reqwest builds the client) blocks the cell. Log out, and the
 * login is refused with `ProxyBlocked` while Settings → Network sits behind
 * the login screen. Without a way out here, the only recovery is editing an
 * AES-GCM encrypted file by hand.
 *
 * The way out is a button, and it works while blocked because
 * `set_proxy_settings` persists before it reconfigures: the disabled settings
 * reach disk whether or not anything else succeeds.
 */
export default function ProxyBlockedBanner() {
  const [reason, setReason] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setReason(blockedReason(await invoke<ProxyStatus>("get_proxy_status")));
    } catch (e) {
      // A status call that fails says nothing about the proxy, and a banner
      // invented from an IPC error would be its own false alarm.
      console.error("Failed to read proxy status:", e);
    }
  }, []);

  useEffect(() => {
    void refresh();
    const onChange = () => void refresh();
    window.addEventListener(PROXY_STATUS_EVENT, onChange);
    return () => window.removeEventListener(PROXY_STATUS_EVENT, onChange);
  }, [refresh]);

  const disableProxy = async () => {
    setBusy(true);
    setActionError(null);
    try {
      // Read the saved settings so host, port and credentials survive — the
      // user is turning the proxy off, not throwing their configuration away.
      // A settings read that fails still disables: a default with
      // `enabled: false` is `Direct`, which is the whole point of the button.
      let settings: ProxySettings = {
        enabled: false,
        proxy_type: "http",
        host: "",
        port: 0,
        username: null,
        password: null,
      };
      try {
        settings = { ...(await invoke<ProxySettings>("get_proxy_settings")) };
      } catch (e) {
        console.error("Failed to read proxy settings:", e);
      }
      settings.enabled = false;
      await invoke("set_proxy_settings", { settings });
    } catch (e) {
      console.error("Failed to disable the proxy:", e);
      setActionError(safeErrorMessage(e, "Could not turn the proxy off"));
    } finally {
      setBusy(false);
      await refresh();
    }
  };

  if (!reason) return null;

  return (
    <div
      role="alert"
      className="flex flex-wrap items-center gap-2.5 px-4 py-2.5 border-b border-[#ff6666]/25 bg-[#ff6666]/10"
    >
      <span className="w-2 h-2 rounded-full flex-shrink-0 bg-[#ff6666]" />
      <span className="min-w-0 text-[11.5px] font-semibold text-[#ff6666] break-words">
        Proxy blocked — nothing can connect. {reason}
      </span>
      {actionError && (
        <span className="min-w-0 text-[11.5px] text-th-text-muted break-words">
          {actionError}
        </span>
      )}
      <button
        onClick={() => void disableProxy()}
        disabled={busy}
        className="ml-auto flex-shrink-0 px-2.5 py-1 rounded-md text-[11.5px] font-semibold border border-[#ff6666]/40 text-th-text-primary hover:bg-[#ff6666]/15 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
      >
        {busy ? "Turning off…" : "Turn off proxy"}
      </button>
    </div>
  );
}
