// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, fireEvent } from "@testing-library/react";

afterEach(cleanup);
import { OperatorModeGate } from "./OperatorModeGate";

describe("operator mode gate", () => {
  it("defaults to human and confirms the pick", () => {
    const onChoose = vi.fn();
    render(<OperatorModeGate onChoose={onChoose} />);
    const [human] = screen.getAllByRole("radio");
    expect(human.getAttribute("aria-checked")).toBe("true");
    fireEvent.click(screen.getByRole("button", { name: /^Continue/ }));
    expect(onChoose).toHaveBeenCalledWith("human");
  });
  it("selecting agent then continuing reports agent", () => {
    const onChoose = vi.fn();
    render(<OperatorModeGate onChoose={onChoose} />);
    const [, agent] = screen.getAllByRole("radio");
    fireEvent.click(agent);
    fireEvent.click(screen.getByRole("button", { name: /^Continue/ }));
    expect(onChoose).toHaveBeenCalledWith("agent");
  });
  it("arrow keys move the selection and Enter confirms", () => {
    const onChoose = vi.fn();
    render(<OperatorModeGate onChoose={onChoose} />);
    const [human, agent] = screen.getAllByRole("radio");
    expect(human.tabIndex).toBe(0);
    expect(agent.tabIndex).toBe(-1);
    fireEvent.keyDown(human, { key: "ArrowDown" });
    expect(agent.getAttribute("aria-checked")).toBe("true");
    expect(document.activeElement).toBe(agent);
    expect(screen.getByRole("button", { name: "Continue as agent" })).toBeTruthy();
    fireEvent.keyDown(agent, { key: "ArrowDown" });
    expect(human.getAttribute("aria-checked")).toBe("true");
    fireEvent.keyDown(human, { key: "ArrowUp" });
    fireEvent.keyDown(agent, { key: "Enter" });
    expect(onChoose).toHaveBeenCalledWith("agent");
  });
});
