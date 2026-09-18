import { useEffect, useRef, useState } from "react";
import { MODELS, type ModelId } from "../data/mock";

export function ModelSelect({
  value,
  onChange,
}: {
  value: ModelId;
  onChange: (id: ModelId) => void;
}) {
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const ref = useRef<HTMLDivElement>(null);
  const selected = MODELS.find((m) => m.id === value)!;

  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", onDoc);
    return () => document.removeEventListener("mousedown", onDoc);
  }, [open]);

  const choose = (i: number) => {
    const m = MODELS[i];
    if (!m.available) return;
    onChange(m.id);
    setOpen(false);
  };

  return (
    <div ref={ref} className="relative">
      <button
        type="button"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label={`Model: ${selected.label}`}
        onClick={() => {
          setOpen(!open);
          setActive(MODELS.findIndex((m) => m.id === value));
        }}
        onKeyDown={(e) => {
          if (e.key === "ArrowDown") {
            e.preventDefault();
            if (!open) setOpen(true);
            else setActive((a) => Math.min(a + 1, MODELS.length - 1));
          } else if (e.key === "ArrowUp") {
            e.preventDefault();
            setActive((a) => Math.max(a - 1, 0));
          } else if (e.key === "Enter" && open) {
            e.preventDefault();
            choose(active);
          } else if (e.key === "Escape") {
            setOpen(false);
          }
        }}
        className="flex h-11 items-center gap-2 rounded-lg border border-border bg-raised px-3 text-sm text-text hover:border-faint"
      >
        <span className="h-1.5 w-1.5 rounded-full bg-accent" aria-hidden="true" />
        {selected.label}
        <svg width="10" height="6" viewBox="0 0 10 6" className={`text-muted motion-safe-fade ${open ? "rotate-180" : ""}`} aria-hidden="true">
          <path d="M1 1l4 4 4-4" stroke="currentColor" strokeWidth="1.5" fill="none" strokeLinecap="round" />
        </svg>
      </button>
      {open && (
        <ul
          role="listbox"
          aria-label="Model"
          className="absolute left-0 top-full z-20 mt-2 w-64 rounded-xl border border-border bg-raised p-1 shadow-xl shadow-black/40"
        >
          {MODELS.map((m, i) => (
            <li
              key={m.id}
              role="option"
              aria-selected={m.id === value}
              aria-disabled={!m.available}
              onClick={() => choose(i)}
              onMouseEnter={() => setActive(i)}
              className={`flex min-h-11 cursor-pointer items-center justify-between rounded-lg px-3 py-2 text-sm ${
                i === active ? "bg-surface" : ""
              } ${m.available ? "text-text" : "cursor-not-allowed text-faint"}`}
            >
              <span>{m.label}</span>
              {m.available ? (
                m.id === value && (
                  <svg width="14" height="14" viewBox="0 0 14 14" className="text-accent" aria-hidden="true">
                    <path d="M2 7.5l3.2 3L12 3.5" stroke="currentColor" strokeWidth="1.6" fill="none" strokeLinecap="round" strokeLinejoin="round" />
                  </svg>
                )
              ) : (
                <span className="text-xs text-faint">{m.reason}</span>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
