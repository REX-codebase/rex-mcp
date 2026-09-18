import { chromium } from "playwright-core";

const BASE = "http://localhost:1420";
const OUT = "/home/sandbox/rex-harness/verification/hero-settle";
const exe = "/usr/bin/google-chrome";
const shot = (p, n) => p.screenshot({ path: `${OUT}/${n}.png` });
const overflow = (p) => p.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
const composerCount = (p) => p.evaluate(() => ({
  hero: document.querySelectorAll("#task-input").length,
  followup: document.querySelectorAll(".followup-input").length,
}));

const browser = await chromium.launch({ executablePath: exe });
const log = [];

// ---------- Desktop 1440 ----------
{
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 2 });
  const p = await ctx.newPage();
  await p.goto(BASE, { waitUntil: "networkidle" });
  await shot(p, "01-idle-1440");
  log.push(["idle overflow", await overflow(p)]);
  log.push(["idle composers", JSON.stringify(await composerCount(p))]);

  await p.fill("#task-input", "Rename every `pexels` import to `stocksnap` across the repo");
  await p.click(".run-button");
  await p.waitForTimeout(240);
  await shot(p, "02-transition-1440");
  log.push(["mid-transition composers", JSON.stringify(await composerCount(p))]);
  await p.waitForTimeout(500);
  await shot(p, "03-settled-working-1440");
  log.push(["settled composers", JSON.stringify(await composerCount(p))]);
  log.push(["settled overflow", await overflow(p)]);

  await p.waitForSelector(".followup-input:not([disabled])", { timeout: 20000 });
  await p.waitForTimeout(400);
  await shot(p, "04-done-1440");
  log.push(["done focused", await p.evaluate(() => document.activeElement?.className ?? "none")]);
  log.push(["done composers", JSON.stringify(await composerCount(p))]);
  log.push(["task quote visible", await p.evaluate(() => !!document.evaluate("//span[contains(.,'pexels')]", document, null, XPathResult.FIRST_ORDERED_NODE_TYPE, null).singleNodeValue)]);

  await p.fill(".followup-input", "Same thing, but keep the old names as deprecated aliases");
  await p.keyboard.press("Control+Enter");
  await p.waitForTimeout(600);
  log.push(["followup busy composers", JSON.stringify(await composerCount(p))]);
  await p.waitForSelector(".followup-input:not([disabled])", { timeout: 20000 });
  await p.waitForTimeout(300);
  await shot(p, "05-followup-done-1440");
  log.push(["followup composers", JSON.stringify(await composerCount(p))]);
  log.push(["turns", await p.evaluate(() => document.querySelectorAll(".turn-request").length)]);
  log.push(["done overflow", await overflow(p)]);

  // New task reset
  await p.click(".newtask-button");
  await p.waitForTimeout(500);
  await shot(p, "06-newtask-1440");
  log.push(["after newtask composers", JSON.stringify(await composerCount(p))]);
  log.push(["hero back", await p.evaluate(() => !!document.querySelector("#task-input"))]);

  // History select from idle -> hero settles away
  await p.click(".history-row");
  await p.waitForTimeout(700);
  await shot(p, "07-history-1440");
  log.push(["history composers", JSON.stringify(await composerCount(p))]);
  log.push(["history overflow", await overflow(p)]);
  await ctx.close();
}

// ---------- Reduced motion ----------
{
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, reducedMotion: "reduce" });
  const p = await ctx.newPage();
  await p.goto(BASE, { waitUntil: "networkidle" });
  await p.fill("#task-input", "Find why the onboarding emails stopped sending");
  await p.click(".run-button");
  await p.waitForTimeout(150);
  await shot(p, "08-reduced-early-1440");
  log.push(["reduced composers @150ms", JSON.stringify(await composerCount(p))]);
  await p.waitForSelector(".followup-input:not([disabled])", { timeout: 20000 });
  await shot(p, "09-reduced-done-1440");
  await ctx.close();
}

// ---------- Mobile 390 ----------
{
  const ctx = await browser.newContext({ viewport: { width: 390, height: 844 }, deviceScaleFactor: 2, isMobile: true, hasTouch: true });
  const p = await ctx.newPage();
  await p.goto(BASE, { waitUntil: "networkidle" });
  await shot(p, "10-idle-390");
  log.push(["mobile idle overflow", await overflow(p)]);
  await p.tap("#task-input");
  await p.fill("#task-input", "Add dark-mode screenshots to the release notes");
  await p.tap(".run-button");
  await p.waitForTimeout(240);
  await shot(p, "11-transition-390");
  await p.waitForSelector(".followup-input:not([disabled])", { timeout: 20000 });
  await p.waitForTimeout(300);
  await shot(p, "12-done-390");
  log.push(["mobile done overflow", await overflow(p)]);
  log.push(["mobile composers", JSON.stringify(await composerCount(p))]);
  await ctx.close();
}

// ---------- Video of the transition ----------
{
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, recordVideo: { dir: "/tmp/rex-video", size: { width: 1280, height: 800 } } });
  const p = await ctx.newPage();
  await p.goto(BASE, { waitUntil: "networkidle" });
  await p.waitForTimeout(800);
  await p.fill("#task-input", "Rename every `pexels` import to `stocksnap` across the repo");
  await p.waitForTimeout(400);
  await p.click(".run-button");
  await p.waitForTimeout(2000);
  await p.waitForSelector(".followup-input:not([disabled])", { timeout: 20000 });
  await p.waitForTimeout(800);
  await p.fill(".followup-input", "Also update the docs that mention it");
  await p.keyboard.press("Control+Enter");
  await p.waitForSelector(".followup-input:not([disabled])", { timeout: 20000 });
  await p.waitForTimeout(600);
  await ctx.close();
}

await browser.close();
console.log(log.map(([k, v]) => `${k}: ${v}`).join("\n"));
