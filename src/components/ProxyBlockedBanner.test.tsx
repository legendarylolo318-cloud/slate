import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import {
  render,
  screen,
  cleanup,
  fireEvent,
  waitFor,
} from "@testing-library/react";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invoke(...args),
}));

import ProxyBlockedBanner, {
  blockedReason,
  PROXY_STATUS_EVENT,
} from "./ProxyBlockedBanner";

beforeEach(() => {
  invoke.mockReset();
});
afterEach(() => cleanup());

describe("blockedReason", () => {
  it("reports only the blocked state", () => {
    expect(blockedReason({ state: "off" })).toBeNull();
    expect(blockedReason({ state: "active", degraded: [] })).toBeNull();
    // Degraded is a per-feature notice, never a bar across the window: the API
    // still works, so the app is usable.
    expect(blockedReason({ state: "active", degraded: ["dash"] })).toBeNull();
    expect(blockedReason({ state: "blocked", reason: "port must not be 0" })).toBe(
      "port must not be 0",
    );
  });

  it("never renders an empty banner", () => {
    // A red bar that says nothing is worse than a generic sentence.
    expect(blockedReason({ state: "blocked", reason: "   " })).toBe(
      "The proxy settings are unusable",
    );
    expect(blockedReason({ state: "blocked" })).toBe(
      "The proxy settings are unusable",
    );
    expect(blockedReason(null)).toBeNull();
    expect(blockedReason("blocked")).toBeNull();
  });
});

describe("the banner", () => {
  it("stays out of the way when nothing is blocked", async () => {
    invoke.mockResolvedValue({ state: "active", degraded: [] });
    render(<ProxyBlockedBanner />);
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("get_proxy_status"));
    expect(screen.queryByRole("alert")).toBeNull();
  });

  /// The state a user can reach and cannot leave: the proxy plans fine, the
  /// client cannot be built, so every request including the login is refused
  /// and the settings screen is behind the login.
  it("reports a block with its reason", async () => {
    invoke.mockResolvedValue({
      state: "blocked",
      reason: "proxy unusable (socks5h://no-such-host.invalid:3128)",
    });
    render(<ProxyBlockedBanner />);
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("no-such-host.invalid");
  });

  it("carries a way out that keeps the rest of the settings", async () => {
    const saved = {
      enabled: true,
      proxy_type: "socks5",
      host: "no-such-host.invalid",
      port: 3128,
      username: "u",
      password: "p",
    };
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "get_proxy_status")
        return Promise.resolve({ state: "blocked", reason: "unusable" });
      if (cmd === "get_proxy_settings") return Promise.resolve(saved);
      return Promise.resolve(undefined);
    });
    render(<ProxyBlockedBanner />);
    fireEvent.click(await screen.findByRole("button"));

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("set_proxy_settings", {
        // Disabled, and otherwise untouched: the user is turning the proxy
        // off, not discarding a configuration they may want back.
        settings: { ...saved, enabled: false },
      }),
    );
  });

  it("still disables when the saved settings cannot be read", async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "get_proxy_status")
        return Promise.resolve({ state: "blocked", reason: "unusable" });
      if (cmd === "get_proxy_settings") return Promise.reject("no settings");
      return Promise.resolve(undefined);
    });
    render(<ProxyBlockedBanner />);
    fireEvent.click(await screen.findByRole("button"));

    await waitFor(() => {
      const call = invoke.mock.calls.find((c) => c[0] === "set_proxy_settings");
      expect(call).toBeTruthy();
      expect((call?.[1] as { settings: { enabled: boolean } }).settings.enabled).toBe(
        false,
      );
    });
  });

  it("clears itself once the block is gone", async () => {
    let blocked = true;
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "get_proxy_status")
        return Promise.resolve(
          blocked
            ? { state: "blocked", reason: "unusable" }
            : { state: "off" },
        );
      return Promise.resolve(undefined);
    });
    render(<ProxyBlockedBanner />);
    await screen.findByRole("alert");

    blocked = false;
    fireEvent(window, new Event(PROXY_STATUS_EVENT));
    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
  });

  it("does not invent a block out of a failed status call", async () => {
    invoke.mockRejectedValue("ipc broke");
    render(<ProxyBlockedBanner />);
    await waitFor(() => expect(invoke).toHaveBeenCalled());
    expect(screen.queryByRole("alert")).toBeNull();
  });
});
