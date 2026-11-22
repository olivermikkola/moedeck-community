# MoE Deck: Multi-Agent Big Goals & Cron Scheduler

MoE Deck coordinates multi-agent goals across **OpenCode**, **Codex**, and **Antigravity (`agy`)**.

```
+-----------------------------------------------------------------------+
|                          MoE Deck Big Goal (Epic)                     |
|  - Title: VM-Lab Phase 3: Autonomous Assessment & Isolation Loop      |
|  - Schedule: Interval or Cron ("15m", "*/30 * * * *", "@hourly")      |
|  - Auto-Triage: Autonomous blocker detection & dynamic subgoal spawn  |
+-----------------------------------+-----------------------------------+
                                    |
          +-------------------------+-------------------------+
          |                         |                         |
+---------v---------+     +---------v---------+     +---------v---------+
| Subgoal 1: OPENCODE|    | Subgoal 2: CODEX  |    | Subgoal 3: AGY    |
| Network namespaces|     | Target VM snapshot|     | Assessment loop   |
| nftables isolation|     | systemd services  |     | Evidence ledger   |
| Boundary proofs   |     | Clean baseline    |     | Retest proofs     |
+-------------------+     +-------------------+     +---------+---------+
                                                              |
                                                              | (Encountered blocker:
                                                              |  PreconditionError /
                                                              |  Network unreachable)
                                                              v
                                                    +-------------------+
                                                    | Dynamic Triage    |
                                                    | Assigned: OPENCODE|
                                                    | Unblock barrier   |
                                                    +-------------------+
```

---

## 1. Hierarchy & Multi-Agent Specialization

1. **Big Goal (Epic)**:
   - Represents an overarching project milestone (e.g. `vm-lab Phase 3`).
   - Tracks overall progress, dependencies, tick schedules, and active blockers.

2. **Subgoals (Tasks)**:
   - Each subgoal is assigned to a specific AI coding agent best suited for that role:
     - **OpenCode (`opencode`)**: Network namespaces, bridge configurations, nftables / iptables rules, firewall isolation proofs.
     - **Codex (`codex`)**: Base image packaging, disposable target VM snapshots, systemd unit staging, filesystem dependencies.
     - **Antigravity (`agy`)**: Assessment-to-retest loops, orchestrator runtime preconditions, service inventories, evidence ledger records.
   - Subgoals support explicit dependencies (`depends_on`), ensuring prerequisites are strictly validated before dependent tasks run.

---

## 2. Autonomous Problem Classification & Dynamic Triage

Automatic triage is off by default. Set `auto_triage: true` explicitly when
creating a goal to enable it. With triage enabled, a failed original subgoal
may create a remediation task within its attempt budget. A failed remediation
ends the goal; it cannot create another remediation. Insufficient credits and
known authentication failures end the goal immediately so the owner can resolve
the account problem.

When an agent executes a subgoal and encounters a failure:
1. **Diagnosis**: The `diagnose_failure` engine inspects the error text, failure signature, or explicit escalation tags (`[PROBLEM: harness=... reason=...]`):
   - **Network / Firewall / Egress / Routing**: Classified as `NetworkIsolation` and routed to **OpenCode**.
   - **VM / Snapshot / libvirt / Disk / systemd**: Classified as `TargetInfrastructure` and routed to **Codex**.
   - **Assertion / Test / Evidence Ledger / Verification**: Classified as `VerificationEvidence` and routed to **Antigravity (`agy`)**.
2. **Dynamic Subgoal Spawning**:
   - Creates a new dynamic triage subgoal (`is_dynamic: true`) targeted to the resolving agent with a precise triage prompt.
   - Marks the failing subgoal as `Blocked`, adding the new triage task to its `depends_on` list.
3. **Resolution & Unblocking**:
   - The scheduler prioritizes dynamic triage tasks first.
   - Once the resolving agent completes the fix (`Passed`), the parent subgoal's blockers are cleared, transitioning it back to `Pending` for automatic retry on the next tick.

---

## 3. Cron & Scheduling Options

### A. In-Daemon Periodic Scheduler
MoE Deck includes an internal background ticker (`GoalManager::start_scheduler`) that wakes up every 15 seconds, finds all active goals whose `next_tick_at` timestamp has arrived, and evaluates them sequentially (preserving the low-impact `one job at a time` rule).

### B. CLI Command
```bash
# List all active Big Goals and subgoals
moedeck goal list

# Trigger an immediate tick on a specific goal
moedeck goal tick <goal_id>

# Inspect subgoals, dependencies, and dynamic triage state
moedeck goal status <goal_id>
```

### C. Headless Cron Script (`scripts/moedeck-cron.py`)
A user-friendly Python runner is provided:
```bash
# Check status of Big Goals
python3 scripts/moedeck-cron.py status

# Trigger evaluation of all active goals
python3 scripts/moedeck-cron.py tick

# Stage the default 3-agent Big Goal for your own vm-lab checkout
python3 scripts/moedeck-cron.py setup-vmlab ~/Projects/vm-lab
```

When ready to schedule via system cron (after review):
```cron
*/15 * * * * nice -n 19 /usr/bin/python3 /path/to/moedeck/scripts/moedeck-cron.py tick >> ~/.config/moedeck/cron.log 2>&1
```

---

## 4. REST API Reference

- `GET /api/goals`: List all Big Goals and their subgoals.
- `POST /api/goals`: Create a new Big Goal.
- `GET /api/goals/:id`: Get detailed goal status, dependencies, and dynamic triage items.
- `POST /api/goals/:id/tick`: Execute one evaluation tick on the goal.
- `POST /api/goals/:id/subgoals`: Add a new subgoal to an existing goal.
- `POST /api/goals/:id/pause`: Pause future goal evaluation (including after restart). An already running turn continues until stopped separately.
- `POST /api/goals/:id/resume`: Resume goal evaluation.
- `DELETE /api/goals/:id`: Remove a goal.
- `POST /api/goals/template/vm-lab`: Stage the standard 3-agent template for `vm-lab`. Requires `workdir` pointing at your own vm-lab checkout.
