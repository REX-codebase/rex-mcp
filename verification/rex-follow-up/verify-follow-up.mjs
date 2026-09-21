import { chromium } from "playwright";
import { mkdir, writeFile } from "node:fs/promises";
import { spawn } from "node:child_process";

const base = "http://127.0.0.1:1420";
const state = process.env.REX_STATE_DIR;
const workspace = process.env.REX_WORKSPACE;
const out = "evidence/rex-follow-up";
if (!state || !workspace) throw new Error("REX_STATE_DIR and REX_WORKSPACE are required");
await mkdir(out, { recursive: true });

const child = spawn("target/debug/rex-mcp", [], {
  env: { ...process.env, REX_STATE_DIR: state, REX_WORKSPACE: workspace },
  stdio: ["pipe", "pipe", "ignore"],
});
let nextId = 0;
let pending = "";
const readResponse = () => new Promise((resolve, reject) => {
  const onData = (chunk) => {
    pending += chunk.toString();
    const lineEnd = pending.indexOf("\n");
    if (lineEnd < 0) return;
    const line = pending.slice(0, lineEnd);
    pending = pending.slice(lineEnd + 1);
    child.stdout.off("data", onData);
    try { resolve(JSON.parse(line)); } catch (error) { reject(error); }
  };
  child.stdout.on("data", onData);
});
const call = async (method, params) => {
  const id = ++nextId;
  child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
  const response = await readResponse();
  if (response.error) throw new Error(JSON.stringify(response.error));
  return response.result?.structuredContent ?? response.result;
};
const tool = (name, arguments_) => call("tools/call", { name, arguments: arguments_ });

await call("initialize", {
  protocolVersion: "2025-11-25",
  capabilities: {},
  clientInfo: { name: "rex-follow-up-pixel-check", version: "1" },
});
const active = await tool("rex_execute", {
  request_id: "pixel-active-follow-up",
  task: "Active follow-up visual task",
  host: "generic_agent",
  operator_is_agent: true,
});
const completed = await tool("rex_execute", {
  request_id: "pixel-completed-follow-up",
  task: "Completed follow-up visual task",
  host: "generic_agent",
  operator_is_agent: true,
});

// Exercise the actual same-task follow-up and rotated handle before completing it.
const resumed = await tool("rex_execute", {
  request_id: "pixel-completed-follow-up-resume",
  task: "Completed follow-up visual task",
  task_id: completed.task_id,
  resume_handle: completed.host_resume_handle,
  follow_up: "Continue this seeded visual task",
  host: "generic_agent",
  operator_is_agent: true,
});
const completedRead = await tool("rex_read", {
  task_id: resumed.task_id,
  lease_epoch: resumed.lease.epoch,
  path: "Cargo.toml",
});
await tool("rex_submit", {
  task_id: resumed.task_id,
  lease_epoch: resumed.lease.epoch,
  action_id: resumed.next.action_id,
  narrative: "Completed after a verified read",
  evidence: { read: completedRead.receipt },
});

const browser = await chromium.launch({ headless: true });
const results = [];
for (const viewport of [
  { name: "desktop-1440x900", width: 1440, height: 900 },
  { name: "phone-390x844", width: 390, height: 844 },
]) {
  const context = await browser.newContext({ viewport, deviceScaleFactor: 1, isMobile: viewport.width < 500 });
  const page = await context.newPage();
  await page.addInitScript(() => localStorage.setItem("rex-operator-mode", "agent"));
  await page.goto(base, { waitUntil: "networkidle" });
  const capture = async (label, taskText) => {
    await page.locator(".rex-task-list button", { hasText: taskText }).click();
    await page.locator(".rex-task").waitFor();
    await page.locator(".rex-follow-up input").waitFor();
    await page.screenshot({ path: `${out}/${label}-${viewport.name}.png`, fullPage: true });
    results.push({
      label,
      viewport: `${viewport.width}x${viewport.height}`,
      width: await page.evaluate(() => document.documentElement.scrollWidth),
      viewportWidth: await page.evaluate(() => window.innerWidth),
      composer: await page.locator(".rex-follow-up input").count(),
      taskId: await page.locator(".rex-task-id").innerText(),
    });
    await page.getByRole("button", { name: "Close supervision" }).click();
  };
  await capture("active", "Active follow-up visual task");
  await capture("completed", "Completed follow-up visual task");
  await context.close();
}
await browser.close();
child.kill();
await writeFile(`${out}/checks.json`, JSON.stringify({ activeTaskId: active.task_id, completedTaskId: completed.task_id, followUpTaskId: resumed.task_id, handleRotated: completed.host_resume_handle !== resumed.host_resume_handle, results }, null, 2));
console.log(JSON.stringify({ activeTaskId: active.task_id, completedTaskId: completed.task_id, followUpTaskId: resumed.task_id, results }, null, 2));