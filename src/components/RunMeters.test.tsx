// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { meterLevel, meterPct, RunMeters, runMeters } from "./RunMeters";

afterEach(cleanup);

describe("run budget meters", () => {
  it("levels at 80% and 100%", () => {
    expect(meterLevel(79, 100)).toBe("ok");
    expect(meterLevel(80, 100)).toBe("warn");
    expect(meterLevel(99, 100)).toBe("warn");
    expect(meterLevel(100, 100)).toBe("full");
    expect(meterLevel(5, 0)).toBe("ok");
  });
  it("clamps the bar", () => {
    expect(meterPct(50, 200)).toBe(25);
    expect(meterPct(300, 200)).toBe(100);
    expect(meterPct(-1, 200)).toBe(0);
    expect(meterPct(1, 0)).toBe(0);
  });
  it("renders all four budgets with values and accessible meters", () => {
    const run = { step: 6, max_steps: 24, tool_calls: 72, max_tool_calls: 80, tokens_used: 18432, max_tokens: 200000, elapsed_ms: 41000, max_wall_ms: 900000 };
    const { container } = render(<RunMeters meters={runMeters(run)} />);
    expect(screen.getByText("6/24")).toBeTruthy();
    expect(screen.getByText("72/80")).toBeTruthy();
    expect(screen.getByText("0:41/15:00")).toBeTruthy();
    const tools = screen.getByRole("meter", { name: "Tools budget" });
    expect(tools.getAttribute("aria-valuenow")).toBe("72");
    expect(tools.getAttribute("aria-valuemax")).toBe("80");
    expect(container.querySelector(".run-meter.is-warn")?.textContent).toContain("Tools");
    expect(container.querySelectorAll(".run-meter.is-warn").length).toBe(1);
    expect((tools.querySelector("i") as HTMLElement).style.width).toBe("90%");
  });
});
