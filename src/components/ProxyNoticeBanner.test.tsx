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

import ProxyNoticeBanner, {
  proxyNotice,
  PROXY_STATUS_EVENT,
  OPEN_SETTINGS_EVENT,
} from "./ProxyNoticeBanner";

beforeEach(() => {
  invoke.mockReset();
});
afterEach(() => cleanup());

describe("proxyNotice", () => {
  it("reports only the states that stop the app working", () => {
    expect(proxyNotice({ state: "off" })).toBeNull();
    expect(proxyNotice({ state: "active", degraded: [] })).toBeNull();
    // Degraded is a per-feature notice, never a bar across the window: the API
    // still works, so the app is usable.
    expect(proxyNotice({ state: "active", degraded: ["dash"] })).toBeNull();
    expect(
      proxyNotice({ state: "blocked", reason: "port must not be 0" })?.detail,
    ).toBe("port must not be 0");
  });

  it("never renders an empty banner", () => {
    // A red bar that says nothing is worse than a generic sentence.
    expect(proxyNotice({ state: "blocked", reason: "   " })?.detail).toBe(
      "The proxy settings are unusable",
    );
    expect(proxyNotice({ state: "blocked" })?.detail).toBe(
      "The proxy settings are unusable",
    );
    expect(proxyNotice(null)).toBeNull();
    expect(proxyNotice("blocked")).toBeNull();
  });

  /// The wording is the requirement, not decoration. reqwest reports a 407 on
  /// the CONNECT tunnel — a mistyped proxy password — with the same kind as a
  /// failed DNS lookup, and an unplugged cable produces the same evidence
  /// again, so the sentence has to survive being true in all three worlds.
  it("says what was observed and never diagnoses a cause it cannot see", () => {
    const notice = proxyNotice({
      state: "unreachable",
      endpoint: "nope.invalid:8080",
    });
    expect(notice?.headline).toBe(
      "SONE isn't getting a reply through the proxy at nope.invalid:8080.",
    );
    // Names the proxy, because that is what the user can act on from here.
    expect(notice?.headline).toContain("nope.invalid:8080");
    // Every cause that produces this evidence, including the one a bare
    // "can't reach it" would misdirect: the proxy answering and refusing.
    expect(notice?.detail).toMatch(/unreachable/i);
    expect(notice?.detail).toMatch(/refusing the connection/i);
    expect(notice?.detail).toMatch(/password/i);
    expect(notice?.detail).toMatch(/offline/i);
    for (const guess of ["is down", "has stopped"]) {
      expect(notice?.headline.toLowerCase()).not.toContain(guess);
      expect(notice?.detail?.toLowerCase()).not.toContain(guess);
    }
  });

  it("still says something useful without an endpoint", () => {
    expect(proxyNotice({ state: "unreachable" })?.headline).toBe(
      "SONE isn't getting a reply through the proxy.",
    );
    expect(
      proxyNotice({ state: "unreachable", endpoint: "  " })?.headline,
    ).toBe("SONE isn't getting a reply through the proxy.");
  });
});

describe("the banner", () => {
  it("stays out of the way when the proxy is working", async () => {
    invoke.mockResolvedValue({ state: "active", degraded: [] });
    render(<ProxyNoticeBanner />);
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("get_proxy_status"),
    );
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
    render(<ProxyNoticeBanner />);
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("no-such-host.invalid");
  });

  /// The common case, and the one that used to render nothing at all: an
  /// `http://` proxy plans and builds even when its host cannot resolve, so
  /// the login failed with a raw DNS error naming the origin rather than the
  /// proxy that never carried the request.
  it("reports an unreachable proxy with the same way out", async () => {
    invoke.mockImplementation((cmd: string) =>
      cmd === "get_proxy_status"
        ? Promise.resolve({
            state: "unreachable",
            endpoint: "nope.invalid:8080",
          })
        : Promise.resolve(undefined),
    );
    render(<ProxyNoticeBanner />);
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("nope.invalid:8080");
    expect(screen.getByRole("button").textContent).toContain("Turn off proxy");
  });

  /// Turning the proxy off removes containment. Where a settings screen is
  /// reachable it must not be the only thing offered — and where it is not
  /// (the login screen), the offer must not appear at all.
  it("offers the non-destructive action only where settings exist", async () => {
    invoke.mockResolvedValue({
      state: "unreachable",
      endpoint: "nope.invalid:8080",
    });
    const { unmount } = render(<ProxyNoticeBanner />);
    await screen.findByRole("alert");
    expect(screen.queryByText("Open network settings")).toBeNull();
    unmount();

    const opened: unknown[] = [];
    const onOpen = (e: Event) => opened.push((e as CustomEvent).detail);
    window.addEventListener(OPEN_SETTINGS_EVENT, onOpen);
    try {
      render(<ProxyNoticeBanner offerSettings />);
      fireEvent.click(await screen.findByText("Open network settings"));
      expect(opened).toEqual(["network"]);
      // And the way out is still there beside it.
      expect(screen.getByText("Turn off proxy")).toBeTruthy();
    } finally {
      window.removeEventListener(OPEN_SETTINGS_EVENT, onOpen);
    }
  });

  it("carries a way out that keeps the rest of the settings", async () => {
    const saved = {
      enabled: true,
      proxy_type: "http",
      host: "nope.invalid",
      port: 8080,
      username: "u",
      password: "p",
    };
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "get_proxy_status")
        return Promise.resolve({
          state: "unreachable",
          endpoint: "nope.invalid:8080",
        });
      if (cmd === "get_proxy_settings") return Promise.resolve(saved);
      return Promise.resolve(undefined);
    });
    render(<ProxyNoticeBanner />);
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
    render(<ProxyNoticeBanner />);
    fireEvent.click(await screen.findByRole("button"));

    await waitFor(() => {
      const call = invoke.mock.calls.find((c) => c[0] === "set_proxy_settings");
      expect(call).toBeTruthy();
      expect(
        (call?.[1] as { settings: { enabled: boolean } }).settings.enabled,
      ).toBe(false);
    });
  });

  it("clears itself once the block is gone", async () => {
    let blocked = true;
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "get_proxy_status")
        return Promise.resolve(
          blocked ? { state: "blocked", reason: "unusable" } : { state: "off" },
        );
      return Promise.resolve(undefined);
    });
    render(<ProxyNoticeBanner />);
    await screen.findByRole("alert");

    blocked = false;
    fireEvent(window, new Event(PROXY_STATUS_EVENT));
    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
  });

  /// Nobody tells the banner that a request has started working again — the
  /// recovery is a request succeeding somewhere else entirely — so it polls
  /// while a proxy is configured, and stops the moment there is nothing a
  /// proxy notice could ever be about.
  it("polls itself back to normal once requests get through again", async () => {
    vi.useFakeTimers();
    try {
      let unreachable = true;
      invoke.mockImplementation((cmd: string) =>
        cmd === "get_proxy_status"
          ? Promise.resolve(
              unreachable
                ? { state: "unreachable", endpoint: "nope.invalid:8080" }
                : { state: "active", degraded: [] },
            )
          : Promise.resolve(undefined),
      );
      render(<ProxyNoticeBanner />);
      await vi.waitFor(() =>
        expect(screen.queryByRole("alert")).not.toBeNull(),
      );

      unreachable = false;
      await vi.advanceTimersByTimeAsync(16000);
      await vi.waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
    } finally {
      vi.useRealTimers();
    }
  });

  it("does not poll while the proxy is off", async () => {
    vi.useFakeTimers();
    try {
      invoke.mockResolvedValue({ state: "off" });
      render(<ProxyNoticeBanner />);
      await vi.waitFor(() => expect(invoke).toHaveBeenCalledTimes(1));
      await vi.advanceTimersByTimeAsync(60000);
      expect(invoke).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("does not invent a notice out of a failed status call", async () => {
    invoke.mockRejectedValue("ipc broke");
    render(<ProxyNoticeBanner />);
    await waitFor(() => expect(invoke).toHaveBeenCalled());
    expect(screen.queryByRole("alert")).toBeNull();
  });
});
