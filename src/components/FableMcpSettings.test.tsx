// @vitest-environment jsdom
import { render, screen, waitFor, fireEvent, cleanup } from "@testing-library/react";
import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";
import { FableMcpSettings } from "./FableMcpSettings";

vi.mock("../data/fable", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../data/fable")>();
  return {
    ...orig,
    fableMcpProbe: vi.fn(),
    loadFableMcpCommand: () => "",
    saveFableMcpCommand: () => {},
  };
});

import { fableMcpProbe } from "../data/fable";

describe("FableMcpSettings", () => {
  beforeEach(() => {
    vi.mocked(fableMcpProbe).mockReset();
  });

  afterEach(() => {
    cleanup();
  });

  it("reports a linked server honestly", async () => {
    vi.mocked(fableMcpProbe).mockResolvedValue({
      available: true,
      server_command: "fable-mcp",
      tools: ["fable_session", "other"],
      has_fable_session_tool: true,
      error: null,
    });
    render(<FableMcpSettings />);
    fireEvent.change(screen.getByLabelText("Server command"), {
      target: { value: "fable-mcp" },
    });
    fireEvent.click(screen.getByText("Probe link"));
    await waitFor(() => {
      expect(screen.getByText(/Linked · fable_session tool found/)).toBeTruthy();
    });
  });

  it("reports a failed link honestly", async () => {
    vi.mocked(fableMcpProbe).mockResolvedValue({
      available: false,
      server_command: "nope",
      tools: [],
      has_fable_session_tool: false,
      error: "cannot spawn nope: not found",
    });
    render(<FableMcpSettings />);
    fireEvent.change(screen.getByLabelText("Server command"), {
      target: { value: "nope" },
    });
    fireEvent.click(screen.getByText("Probe link"));
    await waitFor(() => {
      expect(screen.getByText(/Not linked · native gate only/)).toBeTruthy();
      expect(screen.getByText(/cannot spawn nope/)).toBeTruthy();
    });
  });

  it("says the native gate still works without a server", () => {
    render(<FableMcpSettings />);
    expect(
      screen.getByText(/The Fable gate toggle still works — it uses the native gate/),
    ).toBeTruthy();
  });
});
