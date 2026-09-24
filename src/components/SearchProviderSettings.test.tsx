// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

const list = vi.hoisted(() => ({ items: [] as unknown[] }));
vi.mock("../data/backend", () => ({
  backendKind: () => Promise.resolve("tauri"),
  listSearchProviders: () => Promise.resolve(list.items),
  clearSearchProviderKey: vi.fn(),
  selectSearchProvider: vi.fn(),
  setSearchProviderKey: vi.fn(),
  describeError: String,
}));

import { SearchProviderSettings } from "./SearchProviderSettings";

afterEach(cleanup);

const rex = { id: "rex", name: "REX-search", built_in: true, has_key: true, endpoint: "", docs_url: "" };
const exa = { id: "exa", name: "Exa", built_in: false, has_key: true, endpoint: "api.exa.ai/search", docs_url: "https://exa.ai" };

describe("search provider cards", () => {
  it("the active card has no dead button, the others offer a switch", async () => {
    list.items = [{ ...rex, active: true }, { ...exa, active: false }];
    render(<SearchProviderSettings />);
    await waitFor(() => expect(screen.getByRole("button", { name: "Use provider" })).toBeTruthy());
    expect(screen.queryByRole("button", { name: "Default active" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Use REX-search" })).toBeNull();
  });
  it("when Exa is active, REX-search offers the way back and Exa shows no Active button", async () => {
    list.items = [{ ...rex, active: false }, { ...exa, active: true }];
    render(<SearchProviderSettings />);
    await waitFor(() => expect(screen.getByRole("button", { name: "Use REX-search" })).toBeTruthy());
    expect(screen.queryByRole("button", { name: "Active" })).toBeNull();
    expect(screen.getByRole("button", { name: "Disconnect" })).toBeTruthy();
  });
});
