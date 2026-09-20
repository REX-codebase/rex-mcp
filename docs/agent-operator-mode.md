# Agent-operator mode: task-scoped custody

Status: core kernel implemented (`crates/rex-custody`), 19 lifecycle/failure tests green.
Next: wiring into the autonomous run service, installed-agent route and the Tauri shell.

## The idea

Agents will increasingly arrive at REX the way humans do: with a task and a
promise ("I'll do this"). A promise is not control. Agent-operator mode makes
REX the institution that takes **custody** of the task while someone works
on it - human or agent - and releases it only for a stated reason.

At task entry the operator declares itself:

- **Human operator** - the person at the keyboard (today's behavior).
- **Agent operator** - an external agent. It then picks one of two worker
  paths:
  - **Enter REX as the worker** through a declared protocol boundary
    (`AgentProtocol::Acp` or `AgentProtocol::InstalledCli`). The agent works
    inside REX's workspace, tools and evidence gates.
  - **Stay operator, pick a model** (`WorkerMode::ManagedModel`). REX runs
    its own autonomous loop on the selected provider/model; the external
    agent supervises through heartbeats and receives the evidence bundle.

## The custody lifecycle

```
            offer (host mints: identity, worker, scope, budgets, lease terms, contract)
              |
        commitment handshake  H(offer_hash | nonce | commitment statement)
              |
           ACTIVE <---- heartbeat (monotonic seq, extends lease)
              |   \
   suspend    |    claim completion -> VERIFYING --gates pass--> RELEASED (verified_completion)
   (crash,    |                                  \--gates fail--> ACTIVE (attempt counted)
    lapse)    |                                                       \--attempts exhausted--> QUARANTINED (false_completion)
              |
   resume (per-epoch secret; epoch+1; every secret rotated; old tokens die)
              |
   RELEASED only on: verified completion | explicit failure | budget exhausted
                     | lease expired | human stop | operator cancel
   QUARANTINED on: capability escape | stale lease use | double execution
                   | false completion | nested custody | token forgery | commitment mismatch
```

Every release tombstones the task: no second custody for the same task id
until a human explicitly reopens it.

## The guarantees, and where they live

| Failure mode | Mechanism | Code |
|---|---|---|
| Confused deputy | Workers hold only an unforgeable `CapabilityToken` (random 256-bit secret); privileged transitions are host-only registry methods, unreachable from the tool surface | `registry.rs`, `tools.rs` |
| Privilege widening | Capability set is fixed in the offer; every token carries its hash and is re-verified on every call | `capability.rs` |
| Recursive agent loops | `can_delegate` is structurally ungrantable; no custody-minting capability exists | `registry.rs::offer` |
| Stale lease resurrection | Epoch fencing: resume bumps the epoch and rotates all secrets; old-epoch presentations quarantine | `registry.rs::resume`, `grant_for_token` |
| Replay | Heartbeats require the exact next sequence number | `registry.rs::heartbeat` |
| Duplicate execution | One custody per task id; release tombstones; human-only reopen | `registry.rs` |
| False completion | Completion is decided by evidence gates supplied by the host (`GateEvaluator`), never by the claim; attempts are bounded | `contract.rs`, `registry.rs::claim_completion` |
| Budget laundering via crash | Consumption lives on the grant, persists, and survives resume | `registry.rs::consume` |
| Silent takeover | Human stop works in every phase; approval for risky tools stays with the human via rex-tools (custody never auto-approves) | `registry.rs::human_stop`, `tools.rs` |
| Tampered history | Hash-chained append-only audit log per grant; recovery verifies the chain and fails closed | `audit.rs` |

## Persistence and recovery

`custody/` holds `index.json` (active + tombstones), `grants/<id>.json`
(atomic tmp+rename writes), and `audit/<id>.jsonl`. On startup
`CustodyRegistry::recover` reloads, verifies every audit chain, lapses dead
leases into suspension, and releases suspensions whose grace window passed.
A worker recovering from a crash resumes with its per-epoch resume secret
and receives a fresh token; a second process presenting the old epoch is
treated as resurrection and quarantined.

## Boundaries it keeps

- Consumer subscription OAuth is never a credential here. External agents
  authenticate at their own protocol boundary; REX validates nothing about
  their upstream accounts.
- Unknown protocols fail closed, like the provider adapters.
- Custody narrows authority; it never grants any. Everything a custodied
  worker can do, a human could already do through the normal UI.
