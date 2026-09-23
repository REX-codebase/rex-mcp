// REX VS Code extension — drives the local `rex serve` daemon.
//
// Flow: on activation, spawn `rex serve --port 0` and read the port from
// the daemon's first stdout line ({"port": N}). `REX: Run task…` starts an
// interactive run; the extension polls the run and surfaces plan/tool
// approvals as VS Code modals so the developer never leaves the editor.

import * as vscode from 'vscode';
import { spawn, ChildProcess } from 'child_process';
import * as http from 'http';

let daemon: ChildProcess | null = null;
let port: number | null = null;
let activeRunId: string | null = null;
let pollTimer: NodeJS.Timeout | null = null;
let statusBar: vscode.StatusBarItem;
// Remember which approval we already prompted for, so we ask once per
// pending decision instead of once per poll tick.
let lastPromptKey: string | null = null;

function cfg<T>(key: string, fallback: T): T {
  return vscode.workspace.getConfiguration('rex').get<T>(key, fallback);
}

async function ensureDaemon(): Promise<number> {
  if (port !== null) {
    return port;
  }
  const bin = cfg<string>('rex.path', 'rex');
  return new Promise((resolve, reject) => {
    const child = spawn(bin, ['serve', '--port', '0'], {
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    daemon = child;
    const fail = (msg: string) => {
      daemon = null;
      reject(new Error(msg));
    };
    child.on('error', (e) => fail(`cannot start ${bin}: ${e.message}`));
    let buf = '';
    const onData = (data: Buffer) => {
      buf += data.toString();
      const nl = buf.indexOf('\n');
      if (nl === -1) {
        return;
      }
      const line = buf.slice(0, nl).trim();
      try {
        const parsed = JSON.parse(line) as { port?: number };
        if (typeof parsed.port === 'number') {
          child.stdout?.off('data', onData);
          port = parsed.port;
          resolve(parsed.port);
          return;
        }
      } catch {
        // Not the port line; keep waiting for it.
      }
      buf = buf.slice(nl + 1);
    };
    child.stdout?.on('data', onData);
    child.stderr?.on('data', (d: Buffer) =>
      console.log('[rex]', d.toString().trim())
    );
    setTimeout(() => {
      if (port === null) {
        fail(`timed out waiting for ${bin} serve to announce its port`);
      }
    }, 15000);
  });
}

function api(method: string, path: string, body?: unknown): Promise<any> {
  return ensureDaemon().then(
    (p) =>
      new Promise((resolve, reject) => {
        const payload = body === undefined ? '' : JSON.stringify(body);
        const req = http.request(
          {
            host: '127.0.0.1',
            port: p,
            method,
            path,
            headers: {
              'Content-Type': 'application/json',
              'Content-Length': Buffer.byteLength(payload),
            },
            timeout: 30000,
          },
          (res) => {
            let data = '';
            res.on('data', (c) => (data += c));
            res.on('end', () => {
              let parsed: any = null;
              try {
                parsed = data ? JSON.parse(data) : null;
              } catch {
                parsed = { raw: data };
              }
              if (res.statusCode && res.statusCode >= 400) {
                reject(
                  new Error(parsed?.error || `rex daemon: HTTP ${res.statusCode}`)
                );
              } else {
                resolve(parsed);
              }
            });
          }
        );
        req.on('error', reject);
        req.on('timeout', () => req.destroy(new Error('rex daemon: timed out')));
        if (payload) {
          req.write(payload);
        }
        req.end();
      })
  );
}

function stopPolling() {
  if (pollTimer) {
    clearInterval(pollTimer);
    pollTimer = null;
  }
}

function startPolling() {
  stopPolling();
  pollTimer = setInterval(async () => {
    if (!activeRunId) {
      stopPolling();
      return;
    }
    let snap: any;
    try {
      snap = await api('GET', `/v1/runs/${activeRunId}`);
    } catch (e: any) {
      statusBar.text = `$(error) REX: ${e.message}`;
      return;
    }
    if (!snap.live) {
      const ok = snap.status === 'Completed' ? '$(check)' : '$(error)';
      statusBar.text = `${ok} REX: ${snap.status}`;
      statusBar.tooltip = `Run ${activeRunId} finished: ${snap.status}`;
      activeRunId = null;
      stopPolling();
      return;
    }
    const s = snap.status as string;
    statusBar.text = `$(sync~spin) REX: ${s} · step ${snap.step}/${snap.max_steps}`;
    statusBar.tooltip = `Run ${activeRunId}\n${snap.step}/${snap.max_steps} steps · ${snap.tool_calls} tool calls · ${snap.tokens_used} tokens`;

    // Plan approval: show the plan, approve or deny once.
    if (snap.awaiting_plan) {
      const key = `plan:${activeRunId}`;
      if (lastPromptKey !== key) {
        lastPromptKey = key;
        const plan = (snap.plan as any[])
          .map((p) => `- ${p.title || p.description || JSON.stringify(p)}`)
          .join('\n');
        const choice = await vscode.window.showInformationMessage(
          `REX wants to execute this plan:\n${plan}`,
          { modal: true },
          'Approve plan',
          'Deny'
        );
        await api('POST', `/v1/runs/${activeRunId}/approve`, {
          approve: choice === 'Approve plan',
        });
        lastPromptKey = null;
      }
      return;
    }
    // Tool-call approval.
    if (snap.pending_approval) {
      const pa = snap.pending_approval;
      const key = `tool:${activeRunId}:${pa.call_id}`;
      if (lastPromptKey !== key) {
        lastPromptKey = key;
        const choice = await vscode.window.showWarningMessage(
          `REX wants to run: ${pa.tool}\n${pa.summary}\n(${pa.policy_reason})`,
          { modal: true },
          'Approve',
          'Deny'
        );
        await api('POST', `/v1/runs/${activeRunId}/approve`, {
          approve: choice === 'Approve',
        });
        lastPromptKey = null;
      }
    }
  }, 2000);
}

export function activate(context: vscode.ExtensionContext) {
  statusBar = vscode.window.createStatusBarItem(
    vscode.StatusBarAlignment.Left,
    100
  );
  statusBar.text = 'REX';
  statusBar.tooltip = 'REX harness';
  statusBar.show();
  context.subscriptions.push(statusBar);

  context.subscriptions.push(
    vscode.commands.registerCommand('rex.runTask', async () => {
      const task = await vscode.window.showInputBox({
        prompt: 'What should REX do?',
        placeHolder: 'e.g. add input validation to the login form',
      });
      if (!task || !task.trim()) {
        return;
      }
      try {
        const provider = cfg<string>('rex.provider', '');
        const res = await api('POST', '/v1/runs', {
          task: task.trim(),
          ...(provider ? { provider } : {}),
        });
        activeRunId = res.run_id as string;
        lastPromptKey = null;
        statusBar.text = '$(sync~spin) REX: starting…';
        startPolling();
        vscode.window.showInformationMessage(
          `REX run started: ${(activeRunId as string).slice(0, 18)}…`
        );
      } catch (e: any) {
        vscode.window.showErrorMessage(`REX: ${e.message}`);
      }
    })
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('rex.cancelRun', async () => {
      if (!activeRunId) {
        vscode.window.showInformationMessage('REX: no active run.');
        return;
      }
      try {
        await api('POST', `/v1/runs/${activeRunId}/cancel`);
        vscode.window.showInformationMessage('REX: run cancelled.');
      } catch (e: any) {
        vscode.window.showErrorMessage(`REX: ${e.message}`);
      }
    })
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('rex.showReceipt', async () => {
      if (!activeRunId) {
        vscode.window.showInformationMessage('REX: no active run.');
        return;
      }
      try {
        const receipt = await api('GET', `/v1/runs/${activeRunId}`);
        const doc = await vscode.workspace.openTextDocument({
          content: JSON.stringify(receipt, null, 2),
          language: 'json',
        });
        await vscode.window.showTextDocument(doc);
      } catch (e: any) {
        vscode.window.showErrorMessage(`REX: ${e.message}`);
      }
    })
  );
}

export function deactivate() {
  stopPolling();
  if (daemon) {
    daemon.kill();
    daemon = null;
  }
  port = null;
}
