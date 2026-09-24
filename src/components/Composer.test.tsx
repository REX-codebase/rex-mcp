// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("./ModelStatus", () => ({ ModelStatus: () => null }));

import { Composer, planShortcut } from "./Composer";

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
    fireEvent.click(screen.getByRole("button", { name: /Run options/ }));
    const box = screen.getByLabelText("Summarize old results") as HTMLInputElement;
    expect(box.checked).toBe(false);
    expect(box.closest("label")?.getAttribute("title")).toContain("extra model call");
    fireEvent.click(box);
    expect(set).toHaveBeenCalledWith(true);
  });

  it("is hidden when the caller does not offer it", () => {
    renderComposer();
    fireEvent.click(screen.getByRole("button", { name: /Run options/ }));
    expect(screen.queryByLabelText("Summarize old results")).toBeNull();
    expect(screen.getByLabelText("Fable gate")).toBeTruthy();
  });
});

describe("Composer run mode", () => {
  it("Build/Plan segment reports the chosen mode and relabels Run", () => {
    const set = vi.fn();
    const { rerender } = renderComposer({ setPlanMode: set });
    const build = screen.getByRole("radio", { name: "Build" });
    const plan = screen.getByRole("radio", { name: "Plan" });
    expect(build.getAttribute("aria-checked")).toBe("true");
    expect(plan.getAttribute("aria-checked")).toBe("false");
    expect(screen.getByRole("button", { name: "Run task" })).toBeTruthy();
    fireEvent.click(plan);
    expect(set).toHaveBeenCalledWith(true);
    rerender(<Composer task="t" setTask={() => {}} state="idle" onRun={() => {}} planMode setPlanMode={set} fableGate={false} setFableGate={() => {}} executionPath={path} />);
    expect(screen.getByRole("radio", { name: "Plan" }).getAttribute("aria-checked")).toBe("true");
    expect(screen.getByRole("button", { name: "Plan task" })).toBeTruthy();
    expect(screen.getByText(/nothing runs until you approve the plan/)).toBeTruthy();
  });

  it("Alt+P in the task field flips the mode", () => {
    const set = vi.fn();
    renderComposer({ setPlanMode: set });
    const notPrevented = fireEvent.keyDown(screen.getByLabelText("Task description"), { key: "p", code: "KeyP", altKey: true });
    expect(set).toHaveBeenCalledWith(true);
    // The chord must not also type a character into the task.
    expect(notPrevented).toBe(false);
  });

  it("shortcut matcher ignores other chords and handles macOS Option+P", () => {
    const k = { altKey: true, ctrlKey: false, metaKey: false, shiftKey: false };
    expect(planShortcut({ ...k, key: "p" }, false)).toBe(true);
    expect(planShortcut({ ...k, key: "P" }, true)).toBe(false);
    expect(planShortcut({ ...k, key: "π", code: "KeyP" }, false)).toBe(true);
    expect(planShortcut({ ...k, altKey: false, key: "p" }, false)).toBeNull();
    expect(planShortcut({ ...k, ctrlKey: true, key: "p" }, false)).toBeNull();
    expect(planShortcut({ ...k, metaKey: true, key: "p" }, false)).toBeNull();
    expect(planShortcut({ ...k, shiftKey: true, key: "p" }, false)).toBeNull();
    expect(planShortcut({ ...k, key: "o" }, false)).toBeNull();
  });
});

describe("Composer run options", () => {
  it("counts active extras on the button and closes on Escape", () => {
    renderComposer({ setSummarizeHistory: () => {}, summarizeHistory: true, fableGate: true });
    const btn = screen.getByRole("button", { name: "Run options, 2 on" });
    expect(btn.getAttribute("aria-expanded")).toBe("false");
    fireEvent.click(btn);
    expect(screen.getByRole("dialog", { name: "Run options" })).toBeTruthy();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog", { name: "Run options" })).toBeNull();
  });

  it("shows no count when every extra is off", () => {
    renderComposer({ setSummarizeHistory: () => {} });
    expect(screen.getByRole("button", { name: "Run options" })).toBeTruthy();
  });
});
