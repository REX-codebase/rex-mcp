import { useEffect, useState } from "react";
import {
  costGetBudget,
  costPriceTable,
  costReceipts,
  costSetBudget,
  costSpend,
  formatUsd,
  type Budget,
  type CostReceipt,
  type ModelPrice,
} from "../data/cost";

// Cost panel: price table (estimates), budget sliders, spend tracking,
// and per-run receipts.
export function CostPanel() {
  const [prices, setPrices] = useState<ModelPrice[]>([]);
  const [receipts, setReceipts] = useState<CostReceipt[]>([]);
  const [budget, setBudget] = useState<Budget>({ daily_usd: null, monthly_usd: null });
  const [dailyInput, setDailyInput] = useState("");
  const [monthlyInput, setMonthlyInput] = useState("");
  const [spendDay, setSpendDay] = useState(0);
  const [spendMonth, setSpendMonth] = useState(0);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const refresh = async () => {
    try {
      const [p, r, b, sd, sm] = await Promise.all([
        costPriceTable(),
        costReceipts(20),
        costGetBudget(),
        costSpend(1),
        costSpend(30),
      ]);
      setPrices(p);
      setReceipts(r);
      setBudget(b);
      setDailyInput(b.daily_usd?.toString() ?? "");
      setMonthlyInput(b.monthly_usd?.toString() ?? "");
      setSpendDay(sd);
      setSpendMonth(sm);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    refresh();
  }, []);

  const saveBudget = async () => {
    setError(null);
    const parse = (s: string): number | null => {
      const t = s.trim();
      if (!t) return null;
      const n = parseFloat(t);
      return isNaN(n) || n < 0 ? null : n;
    };
    try {
      const b = { daily_usd: parse(dailyInput), monthly_usd: parse(monthlyInput) };
      await costSetBudget(b);
      setBudget(b);
    } catch (e) {
      setError(String(e));
    }
  };

  if (loading) {
    return (
      <section aria-label="Cost" className="settings-section">
        <h2 className="eyebrow">Cost</h2>
        <p>Loading…</p>
      </section>
    );
  }

  return (
    <section aria-label="Cost" className="settings-section">
      <h2 className="eyebrow">Cost</h2>
      {error && (
        <p className="cost-error" role="alert">
          {error}
        </p>
      )}

      <h3 className="cost-h3">Spend</h3>
      <div className="cost-spend">
        <div>
          <b>{formatUsd(spendDay)}</b>
          <span>last 24h</span>
          {budget.daily_usd !== null && (
            <span className={spendDay > budget.daily_usd ? "over" : ""}>
              of {formatUsd(budget.daily_usd)} budget
            </span>
          )}
        </div>
        <div>
          <b>{formatUsd(spendMonth)}</b>
          <span>last 30 days</span>
          {budget.monthly_usd !== null && (
            <span className={spendMonth > budget.monthly_usd ? "over" : ""}>
              of {formatUsd(budget.monthly_usd)} budget
            </span>
          )}
        </div>
      </div>

      <h3 className="cost-h3">Budgets</h3>
      <div className="cost-budget">
        <label>
          Daily limit (USD)
          <input
            type="number"
            min="0"
            step="0.01"
            value={dailyInput}
            onChange={(e) => setDailyInput(e.target.value)}
            placeholder="No limit"
          />
        </label>
        <label>
          Monthly limit (USD)
          <input
            type="number"
            min="0"
            step="0.01"
            value={monthlyInput}
            onChange={(e) => setMonthlyInput(e.target.value)}
            placeholder="No limit"
          />
        </label>
        <button type="button" className="reset-button" onClick={saveBudget}>
          Save
        </button>
      </div>
      <p className="settings-note">
        Budgets are advisory — REX warns before a run that would exceed them.
      </p>

      <h3 className="cost-h3">Recent runs</h3>
      {receipts.length === 0 ? (
        <p className="cost-empty">No runs recorded yet.</p>
      ) : (
        <ul className="cost-receipts">
          {receipts.map((r) => (
            <li key={r.run_id}>
              <b>{r.model_id}</b>
              <span>
                {r.prompt_tokens.toLocaleString()} in /{" "}
                {r.completion_tokens.toLocaleString()} out
              </span>
              <b>{formatUsd(r.cost_usd)}</b>
              <small>{new Date(r.at_ms).toLocaleString()}</small>
            </li>
          ))}
        </ul>
      )}

      <h3 className="cost-h3">Price table</h3>
      <p className="settings-note">
        Estimates in USD per 1M tokens. Providers change prices — verify
        before relying on these.
      </p>
      <table className="cost-table">
        <thead>
          <tr>
            <th>Model</th>
            <th>Provider</th>
            <th>Input / 1M</th>
            <th>Output / 1M</th>
          </tr>
        </thead>
        <tbody>
          {prices.map((p) => (
            <tr key={p.model_id}>
              <td>
                <code>{p.model_id}</code>
              </td>
              <td>{p.provider}</td>
              <td>${p.input_per_1m.toFixed(2)}</td>
              <td>${p.output_per_1m.toFixed(2)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </section>
  );
}
