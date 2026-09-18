// Sample agent-browser engine.
// There is NO browser runtime in this build. Every page, element, and
// action below is illustrative content rendered so the interface can be
// reviewed. The UI labels it as simulated.

export type PageState = "loading" | "live" | "error";
export type Ownership = "agent" | "user";
export type TrailStatus = "done" | "active" | "pending" | "denied" | "blocked";

export interface TrailEntry {
  id: string;
  at: string;
  verb: string;
  target?: string;
  status: TrailStatus;
}

export interface PermissionAsk {
  action: string;
  detail: string;
}

export interface PageElement {
  id: string;
  label: string;
  role: string;
}

export interface BrowserSession {
  url: string;
  pageTitle: string;
  page: PageState;
  ownership: Ownership;
  intent: string;
  targeting: boolean;
  targetedEl?: string;
  trail: TrailEntry[];
  permission: PermissionAsk | null;
  shots: string[];
  receipt: string;
  done: boolean;
  blockedReason?: string;
}

export const HOME_URL = "https://cornitos.example/shop/sizzlin-jalapeno";
export const CART_URL = "https://cornitos.example/cart";

export const PAGE_ELEMENTS: PageElement[] = [
  { id: "product-title", label: "Product title", role: "heading" },
  { id: "price", label: "Price", role: "text" },
  { id: "pack-size", label: "Pack size", role: "text" },
  { id: "add-to-cart", label: "Add to cart", role: "button" },
  { id: "qty-stepper", label: "Quantity", role: "stepper" },
];

const INTENT = "Open the Sizzlin' Jalapeno page, confirm the price, and stage one pack in the cart.";
const RECEIPT = "B-0119";

export function newBrowserSession(): BrowserSession {
  return {
    url: "about:blank",
    pageTitle: "Blank page",
    page: "loading",
    ownership: "agent",
    intent: INTENT,
    targeting: false,
    trail: [],
    permission: null,
    shots: [],
    receipt: RECEIPT,
    done: false,
  };
}

let seq = 0;
const tid = () => `e${++seq}`;
const now = () => "now";

// Local mock engine: walks a browser session through truthful UI states
// with sample content. It simulates pacing only; it never touches a
// network, a real page, credentials, or a browser.
export function startBrowserPreview(
  onUpdate: (s: BrowserSession) => void,
  reducedMotion: boolean
): {
  allow: () => void;
  deny: () => void;
  retry: () => void;
  setOwnership: (o: Ownership) => void;
  setTargeting: (on: boolean) => void;
  pickElement: (id: string) => void;
  followUp: (text: string) => void;
  cancel: () => void;
} {
  const timers: ReturnType<typeof setTimeout>[] = [];
  const s = newBrowserSession();
  const gap = reducedMotion ? 300 : 1050;
  const later = (fn: () => void, ms: number) => timers.push(setTimeout(fn, ms));
  const emit = () => onUpdate({ ...s, trail: [...s.trail], shots: [...s.shots] });
  const log = (verb: string, target: string | undefined, status: TrailStatus) => {
    s.trail = [...s.trail, { id: tid(), at: now(), verb, target, status }];
  };
  const settleActive = (status: TrailStatus = "done") => {
    s.trail = s.trail.map((t) => (t.status === "active" ? { ...t, status } : t));
  };

  // Act 1: navigate and read the product page.
  s.page = "loading";
  log("Navigate", HOME_URL, "active");
  emit();
  later(() => {
    settleActive();
    s.url = HOME_URL;
    s.pageTitle = "Sizzlin' Jalapeno Nacho Crisps - 150 g";
    s.page = "live";
    log("Read", "product-title", "active");
    emit();
  }, gap * 1.4);
  later(() => {
    settleActive();
    log("Extract", "price -> Rs 45", "active");
    s.shots = ["product"];
    emit();
  }, gap * 2.4);
  later(() => {
    settleActive();
    log("Wait for permission", "click add-to-cart", "pending");
    s.permission = {
      action: "Click \"Add to cart\"",
      detail: "This stages one 150 g pack in a sample cart. No purchase, no payment, no real store.",
    };
    emit();
  }, gap * 3.4);

  // Act 2 (after permission): cart navigation hits a sample error, then recovers.
  const runCartLeg = () => {
    s.page = "loading";
    s.url = CART_URL;
    s.targetedEl = undefined;
    s.targeting = false;
    s.pageTitle = "Cart";
    log("Navigate", CART_URL, "active");
    emit();
    later(() => {
      settleActive();
      s.page = "error";
      s.pageTitle = "Cart - failed to load";
      log("Load cart", "sample network error", "blocked");
      emit();
    }, gap * 1.3);
  };

  const finish = () => {
    settleActive();
    log("Extract", "cart total -> Rs 45", "active");
    s.shots = [...s.shots, "cart"];
    emit();
    later(() => {
      settleActive();
      s.done = true;
      s.intent = "Done. Price confirmed and one pack staged in the sample cart.";
      s.shots = [...s.shots, "receipt"];
      log("Receipt", `${RECEIPT} written`, "done");
      emit();
    }, gap);
  };

  const allow = () => {
    if (!s.permission || s.ownership !== "agent") return;
    s.permission = null;
    s.trail = s.trail.map((t) => (t.status === "pending" ? { ...t, status: "done" } : t));
    log("Click", "add-to-cart (allowed once)", "active");
    emit();
    later(() => {
      settleActive();
      runCartLeg();
    }, gap * 0.8);
  };

  const deny = () => {
    if (!s.permission) return;
    s.permission = null;
    s.trail = s.trail.map((t) => (t.status === "pending" ? { ...t, status: "denied" } : t));
    s.done = true;
    s.blockedReason = "Permission denied. The agent stopped before the click; the price it already read stays in the trail.";
    s.intent = "Stopped: you denied the click. Nothing was staged.";
    log("Stopped", "permission denied", "blocked");
    emit();
  };

  const retry = () => {
    if (s.page !== "error") return;
    s.page = "loading";
    log("Retry", CART_URL, "active");
    emit();
    later(() => {
      settleActive();
      s.page = "live";
      s.targetedEl = undefined;
      s.targeting = false;
      s.pageTitle = "Cart (1 item)";
      log("Read", "cart contents", "active");
      emit();
      later(finish, gap);
    }, gap * 1.2);
  };

  const setOwnership = (o: Ownership) => {
    if (s.ownership === o || s.done) return;
    s.ownership = o;
    s.targeting = false;
    log(o === "user" ? "Handover" : "Handback", o === "user" ? "you have control" : "agent has control", "done");
    emit();
  };

  const setTargeting = (on: boolean) => {
    if (s.page !== "live" || s.ownership !== "agent" || s.done) return;
    s.targeting = on;
    if (!on) s.targetedEl = undefined;
    emit();
  };

  const pickElement = (id: string) => {
    if (!s.targeting) return;
    const el = PAGE_ELEMENTS.find((e) => e.id === id);
    if (!el) return;
    s.targetedEl = id;
    s.targeting = false;
    log("Target", `#${id} (${el.label.toLowerCase()})`, "done");
    emit();
  };

  const followUp = (text: string) => {
    if (s.done && !s.blockedReason) {
      s.intent = text;
      log("Follow-up", `"${text}"`, "active");
      emit();
      later(() => {
        settleActive();
        log("Answer", "handled with this session's context (sample)", "done");
        s.intent = INTENT;
        emit();
      }, gap);
    }
  };

  const cancel = () => timers.forEach(clearTimeout);
  return { allow, deny, retry, setOwnership, setTargeting, pickElement, followUp, cancel };
}
