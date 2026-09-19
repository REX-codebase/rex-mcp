import { chromium } from 'playwright';
import fs from 'node:fs';
const out = 'verification/motion-settings';
const URL_BASE = 'http://127.0.0.1:4173';
const checks = {};
const browser = await chromium.launch({ headless: true });

const shell = (page) => page.locator('.harness-shell');
const classes = (page) => shell(page).getAttribute('class');
const heroAnimMs = (page) => page.locator('.hero-settle-inner').evaluate((el) => parseFloat(getComputedStyle(el).animationDuration) * 1000);
const goSettings = async (page) => { await page.getByRole('button', { name: 'Settings', exact: true }).click(); };
const pickMotion = async (page, label) => { await page.getByRole('radio', { name: label, exact: true }).click(); };

// 1. Default: System mode, OS no-preference -> full motion.
let page = await browser.newPage({ viewport: { width: 1280, height: 820 } });
await page.goto(URL_BASE);
checks.defaultMode = await shell(page).getAttribute('data-motion');
checks.defaultHasReducedFx = (await classes(page)).includes('reduced-fx');
checks.defaultHeroAnimMs = await heroAnimMs(page);

// 2. Persistence: choose Reduce, reload, still Reduce.
await goSettings(page);
await page.screenshot({ path: `${out}/01-settings-default.png` });
await pickMotion(page, 'Reduce');
checks.reduceAriaChecked = await page.getByRole('radio', { name: 'Reduce', exact: true }).getAttribute('aria-checked');
checks.reduceStored = await page.evaluate(() => window.localStorage.getItem('rex-harness-motion'));
await page.reload();
checks.reducePersistsAfterReload = await shell(page).getAttribute('data-motion');
checks.reduceHasReducedFxAfterReload = (await classes(page)).includes('reduced-fx');

// 3. Reset returns to System and clears storage.
await goSettings(page);
await page.getByRole('button', { name: 'Reset to defaults' }).click();
checks.resetMode = await shell(page).getAttribute('data-motion');
checks.resetStored = await page.evaluate(() => window.localStorage.getItem('rex-harness-motion'));
await page.close();

// 4. System mode follows live OS changes without reload.
page = await browser.newPage({ viewport: { width: 1280, height: 820 } });
await page.goto(URL_BASE);
checks.systemBeforeOsFlip = (await classes(page)).includes('reduced-fx');
await page.emulateMedia({ reducedMotion: 'reduce' });
await page.waitForTimeout(150);
checks.systemAfterOsReduce = (await classes(page)).includes('reduced-fx');
checks.systemAfterOsReduceAnimMs = await heroAnimMs(page);
await page.screenshot({ path: `${out}/02-system-os-reduce-live.png` });
await page.emulateMedia({ reducedMotion: 'no-preference' });
await page.waitForTimeout(150);
checks.systemAfterOsNoPreference = (await classes(page)).includes('reduced-fx');
checks.systemModeStaysSystem = await shell(page).getAttribute('data-motion');
await page.close();

// 5. Full overrides an OS-level reduce: complete motion, transitions fire.
page = await browser.newPage({ viewport: { width: 1280, height: 820 }, reducedMotion: 'reduce' });
await page.goto(URL_BASE);
await goSettings(page);
await pickMotion(page, 'Full');
await page.getByRole('button', { name: 'Task', exact: true }).click();
checks.fullWithOsReduceClass = (await classes(page)).includes('reduced-fx');
checks.fullWithOsReduceAnimMs = await heroAnimMs(page);
await page.getByRole('button', { name: /Fast mode/i }).click();
await page.waitForTimeout(120);
checks.fullFastTransitionCount = await page.locator('.fast-transition').count();
await page.screenshot({ path: `${out}/03-full-overrides-os-reduce.png` });
await page.waitForTimeout(1100);
await page.close();

// 6. Reduce with OS no-preference: calm but usable - no fast transition, menu suppressed.
page = await browser.newPage({ viewport: { width: 1280, height: 820 } });
await page.goto(URL_BASE);
await goSettings(page);
await pickMotion(page, 'Reduce');
await page.getByRole('button', { name: 'Task', exact: true }).click();
checks.reduceWithOsFullAnimMs = await heroAnimMs(page);
await page.getByRole('button', { name: /Fast mode/i }).click();
await page.waitForTimeout(120);
checks.reduceFastTransitionCount = await page.locator('.fast-transition').count();
checks.reduceFastPressed = await page.getByRole('button', { name: /Fast mode/i }).getAttribute('aria-pressed');
await page.locator('.model-trigger').first().click();
await page.waitForTimeout(300);
const menu = page.locator('.provider-menu, .model-menu').first();
checks.reduceMenuVisible = await menu.isVisible();
checks.reduceMenuAnimMs = await menu.evaluate((el) => parseFloat(getComputedStyle(el).animationDuration) * 1000);
await page.keyboard.press('Escape');
await page.screenshot({ path: `${out}/04-reduce-calm.png` });
await page.close();

// 7. Full: menu animation intact (intended complete motion).
page = await browser.newPage({ viewport: { width: 1280, height: 820 } });
await page.goto(URL_BASE);
await goSettings(page);
await pickMotion(page, 'Full');
await page.getByRole('button', { name: 'Task', exact: true }).click();
await page.locator('.model-trigger').first().click();
await page.waitForTimeout(60);
const menuFull = page.locator('.provider-menu, .model-menu').first();
checks.fullMenuVisible = await menuFull.isVisible();
checks.fullMenuAnimMs = await menuFull.evaluate((el) => parseFloat(getComputedStyle(el).animationDuration) * 1000);
await page.close();

await browser.close();
fs.writeFileSync(`${out}/checks.json`, JSON.stringify(checks, null, 2));
console.log(JSON.stringify(checks, null, 2));
