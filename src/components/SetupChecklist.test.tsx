// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { SetupChecklist } from "./SetupChecklist";

afterEach(cleanup);

const props = { tour: false, onTour: () => {}, onOpenSettings: () => {}, onRecheck: () => {} };

describe("setup checklist", () => {
  it("scrolls itself into view when Run lights it up", () => {
    const scroll = vi.fn();
    Element.prototype.scrollIntoView = scroll;
    const { rerender } = render(<SetupChecklist {...props} hot={false} />);
    expect(scroll).not.toHaveBeenCalled();
    rerender(<SetupChecklist {...props} hot />);
    expect(scroll).toHaveBeenCalledTimes(1);
    expect(screen.getByLabelText("Connect a backend").className).toContain("attn");
  });
  it("uses plain copy that points at the composer above it", () => {
    render(<SetupChecklist {...props} hot={false} />);
    expect(screen.getByText(/Describe a task above/)).toBeTruthy();
    expect(screen.queryByText(/0600/)).toBeNull();
  });
});
