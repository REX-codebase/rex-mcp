// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("./ModelStatus", () => ({ ModelStatus: () => null }));

import { Composer } from "./Composer";

const path = { kind: "managed", label: "Managed", detail: "" } as never;

function renderComposer(extra: Record<string, unknown> = {}) {
  return render(
    <Composer
      task="t"
      setTask={() => {}}
      state="idle"
      onRun={() => {}}
      planMode={false}
      setPlanMode={() => {}}
      fableGate={false}
      setFableGate={() => {}}
      executionPath={path}
      {...extra}
    />,
  );
}

afterEach(cleanup);

describe("Composer history-summary switch", () => {
  it("is off by default and reports changes", () => {
    const set = vi.fn();
    renderComposer({ setSummarizeHistory: set });
    const box = screen.getByLabelText("Summarize old results") as HTMLInputElement;
    expect(box.checked).toBe(false);
    expect(box.closest("label")?.getAttribute("title")).toContain("extra model call");
    fireEvent.click(box);
    expect(set).toHaveBeenCalledWith(true);
  });

  it("is hidden when the caller does not offer it", () => {
    renderComposer();
    expect(screen.queryByLabelText("Summarize old results")).toBeNull();
  });
});
