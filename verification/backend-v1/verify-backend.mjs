import { chromium } from 'playwright';
import { mkdir, writeFile } from 'node:fs/promises';

// Verifies the model dropdown is driven by the real Rust backend
// (rex-dev-server sidecar on 127.0.0.1:8787, replaying the live-recorded
// Gemini catalog), plus truthful error and preview fallback states.
const base = 'http://localhost:1420';
await mkdir('verification/backend-v1', { recursive: true });
const browser = await chromium.launch({ headless: true });
const results = {};

const context = await browser.newContext({
  viewport: { width: 1280, height: 800 },
  recordVideo: { dir: 'verification/backend-v1/videos', size: { width: 1280, height: 800 } },
});
const page = await context.newPage();
page.on('console', (m) => { if (m.type() === 'error') results.consoleError = m.text(); });
await page.goto(base, { waitUntil: 'networkidle' });

// 1. Trigger shows live availability from the backend.
await page.getByRole('button', { name: /Choose model/ }).waitFor();
results.triggerLabel = await page.getByRole('button', { name: /Choose model/ }).innerText();
await page.getByRole('button', { name: /Choose model/ }).click();
await page.getByRole('menu').waitFor();
await page.waitForTimeout(400);
results.menuHeader = await page.locator('.provider-menu-head b').innerText();
results.sidecarNote = await page.getByText(/DEV · Rust sidecar/).isVisible();
await page.screenshot({ path: 'verification/backend-v1/01-menu-live-models.png' });

// 2. Real models from the (recorded live) Gemini catalog are listed.
const geminiSection = page.getByRole('region', { name: 'Google Gemini models' }).or(page.locator('.provider-block'));
results.modelCount = await page.locator('.provider-block .model-option').count();
results.firstModels = await page.locator('.provider-block .model-option b').allInnerTexts().then((t) => t.slice(0, 3));

// 3. Select a real model; the trigger must reflect it.
await page.locator('.provider-block .model-option', { hasText: 'gemini-3-flash-preview' }).first().click();
await page.getByRole('menu').waitFor({ state: 'detached' });
results.selectedLabel = await page.locator('.model-trigger b').innerText();
await page.screenshot({ path: 'verification/backend-v1/02-model-selected.png' });

// 4. Connect API dialog: bad key -> real provider 401 -> truthful error.
await page.getByRole('button', { name: /Choose model|gemini-3-flash-preview/ }).click();
await page.getByRole('menuitem', { name: /Connect API/ }).click();
const dialog = page.getByRole('dialog');
await dialog.waitFor();
await dialog.getByLabel('Provider').selectOption('anthropic');
await dialog.getByLabel('API key').fill('definitely-not-a-real-key');
await page.screenshot({ path: 'verification/backend-v1/03-connect-dialog.png' });
await dialog.getByRole('button', { name: /Save key & fetch models/ }).click();
await page.getByRole('alert').waitFor({ timeout: 30000 });
results.badKeyError = await page.getByRole('alert').innerText();
await page.screenshot({ path: 'verification/backend-v1/04-auth-error.png' });
await dialog.getByRole('button', { name: 'Close connection dialog' }).click();

// 5. Reopen the menu: Anthropic now shows its truthful auth error, Gemini still lists live models.
await page.getByRole('button', { name: /Choose model|gemini-3-flash-preview/ }).click();
await page.getByRole('menu').waitFor();
await page.waitForTimeout(500);
results.anthropicErrorShown = await page.getByText(/rejected the API key/).first().isVisible();
results.geminiStillListed = await page.locator('.provider-block .model-option').count();
await page.screenshot({ path: 'verification/backend-v1/05-menu-with-error-state.png' });
await page.keyboard.press('Escape');

// 6. Mobile viewport sanity.
await page.setViewportSize({ width: 390, height: 760 });
await page.goto(base, { waitUntil: 'networkidle' });
await page.getByRole('button', { name: /Choose model/ }).click();
await page.getByRole('menu').waitFor();
await page.screenshot({ path: 'verification/backend-v1/06-mobile-menu.png' });
results.mobileOverflow = await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth + 1);

await context.close();

// 7. No-backend fallback: block the sidecar and confirm the preview state.
const context2 = await browser.newContext({ viewport: { width: 1280, height: 800 } });
const page2 = await context2.newPage();
await page2.route('http://127.0.0.1:8787/**', (route) => route.abort());
await page2.goto(base, { waitUntil: 'networkidle' });
await page2.getByRole('button', { name: /Choose model/ }).click();
await page2.getByRole('menu').waitFor();
results.previewFallback = await page2.getByText('PREVIEW · Connections are not active').isVisible();
await page2.screenshot({ path: 'verification/backend-v1/07-preview-fallback.png' });
await context2.close();

await browser.close();
await writeFile('verification/backend-v1/checks.json', JSON.stringify(results, null, 2));
console.log(JSON.stringify(results, null, 2));
