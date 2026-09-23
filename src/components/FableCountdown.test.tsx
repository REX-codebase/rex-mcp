// @vitest-environment jsdom
import { render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { FableCountdown } from "./FableCountdown";
import type { FableStatus } from "../data/fable";

vi.mock("../data/fable", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../data/fable")>();
  return { ...orig, fableSessionStatus: vi.fn() };
});

import { fableSessionStatus } from "../data/fable";

const running: FableStatus = {
  name: "demo",
  objective: "fix auth",
  phase: "PROVE",
  unlocked: false,
  timer_remaining_ms: 2_520_000,
  timer_remaining_human: "42m 00s",
  timer_elapsed: false,
  proven_count: 2,
  invariant_count: 1,
  unlock_ready: false,
};

const elapsed: FableStatus = {
  ...running,
  timer_remaining_ms: 0,
  timer_remaining_human: "0s",
  timer_elapsed: true,
  unlock_ready: true,
};

describe("FableCountdown", () => {
  it("shows the live countdown and evidence counts while the timer runs", async () => {
    vi.mocked(fableSessionStatus).mockResolvedValue(running);
    render(<FableCountdown sessionName="demo" />);
    await waitFor(() => {
      expect(screen.getByText("42m 00s")).toBeTruthy();
    });
    expect(screen.getByText(/Deliberation lock/)).toBeTruthy();
    expect(screen.getByText(/2 PROVEN/)).toBeTruthy();
  });

  it("shows the elapsed state once the gate opens", async () => {
    vi.mocked(fableSessionStatus).mockResolvedValue(elapsed);
    render(<FableCountdown sessionName="demo" />);
    await waitFor(() => {
      expect(screen.getByText(/Authority timer elapsed/)).toBeTruthy();
    });
  });

  it("reports backend read failures honestly", async () => {
    vi.mocked(fableSessionStatus).mockRejectedValue(new Error("no such session"));
    render(<FableCountdown sessionName="demo" />);
    await waitFor(() => {
      expect(screen.getByText(/Could not read the timer/)).toBeTruthy();
    });
  });
});
