// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("../data/backend", () => ({ backendKind: () => Promise.resolve("mock"), listInstalledAgents: () => Promise.resolve([]) }));

import { InstalledAgentsSettings } from "./InstalledAgentsSettings";

afterEach(cleanup);

describe("installed agent backends settings", () => {
  it("keeps the terms and review facts, folded behind a disclosure", () => {
    const { container } = render(<InstalledAgentsSettings />);
    const details = container.querySelector("details.settings-more")!;
    expect(details.hasAttribute("open")).toBe(false);
    expect(screen.getByText("Why only Codex CLI, and how REX checks it")).toBeTruthy();
    expect(details.textContent).toContain("removed on 19 Sep 2026");
    expect(details.textContent).toContain("fails closed");
  });
  it("names the one supported CLI outside the desktop app", () => {
    render(<InstalledAgentsSettings />);
    expect(screen.getByText("OpenAI Codex CLI")).toBeTruthy();
    expect(screen.getByText("Detected in the desktop app")).toBeTruthy();
  });
});
