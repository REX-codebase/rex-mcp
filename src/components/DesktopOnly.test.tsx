// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { DesktopOnly, isDesktopRuntime } from "./DesktopOnly";

afterEach(cleanup);

describe("desktop-only tabs", () => {
  it("detects the Tauri bridge", () => {
    expect(isDesktopRuntime({ __TAURI_INTERNALS__: {} })).toBe(true);
    expect(isDesktopRuntime({})).toBe(false);
    expect(isDesktopRuntime(undefined)).toBe(false);
    expect(isDesktopRuntime(window)).toBe(false);
  });
  it("explains what each tab needs instead of a raw error", () => {
    render(<DesktopOnly feature="git" />);
    expect(screen.getByRole("heading", { name: "Git and checkpoints run in the desktop app" })).toBeTruthy();
    expect(screen.getByText("npm run tauri dev")).toBeTruthy();
    expect(document.body.textContent).not.toMatch(/TypeError|undefined/);
  });
});
