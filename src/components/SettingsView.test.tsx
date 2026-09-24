// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("./CostPanel", () => ({ CostPanel: () => <p>cost-panel-live</p> }));
vi.mock("./McpServerPanel", () => ({ McpServerPanel: () => null }));
vi.mock("./FableMcpSettings", () => ({ FableMcpSettings: () => null }));
vi.mock("./SearchProviderSettings", () => ({ SearchProviderSettings: () => null }));
vi.mock("./InstalledAgentsSettings", () => ({ InstalledAgentsSettings: () => null }));

import { SettingsView } from "./SettingsView";

afterEach(() => {
  cleanup();
  delete (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
});

const renderIt = () => render(<SettingsView motion="system" setMotion={() => {}} onReset={() => {}} />);

describe("Settings cost section", () => {
  it("explains where cost data lives in the browser preview", () => {
    renderIt();
    expect(screen.queryByText("cost-panel-live")).toBeNull();
    expect(screen.getByText(/local cost.json file/)).toBeTruthy();
  });
  it("shows the live cost panel inside the desktop app", () => {
    (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ = {};
    renderIt();
    expect(screen.getByText("cost-panel-live")).toBeTruthy();
    expect(screen.queryByText(/local cost.json file/)).toBeNull();
  });
  it("lists the Build/Plan shortcut", () => {
    renderIt();
    expect(screen.getByText("Alt + P")).toBeTruthy();
  });
  it("documents the approval keys", () => {
    renderIt();
    expect(screen.getByText("Close a menu or dialog; deny an approval or reject a plan")).toBeTruthy();
    expect(screen.getByText("Run the task, send a follow-up, or send a typed answer")).toBeTruthy();
  });
});
