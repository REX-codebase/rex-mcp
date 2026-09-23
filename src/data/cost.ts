// Cost visibility bindings: price tables, receipts, budgets.

import { invoke } from "@tauri-apps/api/core";

export interface ModelPrice {
  model_id: string;
  provider: string;
  input_per_1m: number;
  output_per_1m: number;
  as_of: string;
}

export interface CostReceipt {
  run_id: string;
  model_id: string;
  prompt_tokens: number;
  completion_tokens: number;
  cost_usd: number | null;
  at_ms: number;
}

export interface Budget {
  daily_usd: number | null;
  monthly_usd: number | null;
}

export async function costPriceTable(): Promise<ModelPrice[]> {
  return invoke<ModelPrice[]>("cost_price_table");
}

export async function costReceipts(limit: number): Promise<CostReceipt[]> {
  return invoke<CostReceipt[]>("cost_receipts", { limit });
}

export async function costGetBudget(): Promise<Budget> {
  return invoke<Budget>("cost_get_budget");
}

export async function costSetBudget(budget: Budget): Promise<void> {
  return invoke<void>("cost_set_budget", { budget });
}

export async function costSpend(days: number): Promise<number> {
  return invoke<number>("cost_spend", { days });
}

export function formatUsd(usd: number | null | undefined): string {
  if (usd === null || usd === undefined) return "—";
  if (usd < 0.01) return `$${usd.toFixed(4)}`;
  return `$${usd.toFixed(2)}`;
}
