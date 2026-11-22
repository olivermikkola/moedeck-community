#!/usr/bin/env python3
"""
moedeck-cron.py — Headless Cron & Periodic Scheduler for MoE Deck

Coordinates multi-agent goals across OpenCode, Codex, and Antigravity (Agy):
- Big Goal: Overarching project epic (e.g. vm-lab Phase 3 loop)
- Subgoals: One large task per agent (OpenCode -> Codex -> Agy)
- Autonomous Dynamic Triage: Automatically detects blockers/failures and
  spawns targeted triage subgoals to resolve network barriers, missing snapshots,
  or verification failures before retrying.

Usage:
  python3 scripts/moedeck-cron.py status           # Show active Big Goals & subgoals
  python3 scripts/moedeck-cron.py tick             # Evaluate due goals sequentially
  python3 scripts/moedeck-cron.py setup-vmlab      # Stage the 3-agent Big Goal for vm-lab
"""

import sys
import json
import urllib.request
import urllib.error
import os

MOEDECK_PORT = int(os.environ.get("MOEDECK_PORT", "7770"))
BASE_URL = f"http://127.0.0.1:{MOEDECK_PORT}"

def load_offline_goals():
    for p in [
        os.path.expanduser("~/.config/moedeck/goals.json"),
        os.path.expanduser("~/.config/helm/goals.json"),
    ]:
        if os.path.exists(p):
            try:
                with open(p, "r", encoding="utf-8") as f:
                    data = json.load(f)
                    return list(data.values())
            except Exception:
                pass
    return []

def api_get(endpoint: str):
    url = f"{BASE_URL}{endpoint}"
    req = urllib.request.Request(url, headers={"User-Agent": "moedeck-cron/1.0"})
    with urllib.request.urlopen(req, timeout=10) as resp:
        return json.loads(resp.read().decode())

def api_post(endpoint: str, data: dict = None):
    url = f"{BASE_URL}{endpoint}"
    payload = json.dumps(data or {}).encode()
    req = urllib.request.Request(
        url,
        data=payload,
        headers={"Content-Type": "application/json", "User-Agent": "moedeck-cron/1.0"},
        method="POST"
    )
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            return json.loads(resp.read().decode())
    except urllib.error.HTTPError as e:
        body = e.read().decode()
        print(f"API Error {e.code}: {body}")
        sys.exit(1)
    except urllib.error.URLError as e:
        print(f"Error connecting to MoE Deck daemon at {BASE_URL}: {e}")
        sys.exit(1)

def cmd_status():
    try:
        data = api_get("/api/goals")
        goals = data.get("goals", [])
    except Exception:
        goals = load_offline_goals()
        if goals:
            print("Note: MoE Deck daemon offline / unreachable; displaying on-disk goals directly.")
    if not goals:
        print("No Big Goals currently defined in MoE Deck.")
        print("Run `python3 scripts/moedeck-cron.py setup-vmlab` to stage the vm-lab tri-agent goal.")
        return

    print("=" * 70)
    print("  MoE Deck Multi-Agent Goals")
    print("=" * 70)
    for g in goals:
        status_badge = {
            "queued": "⏳ QUEUED",
            "running": "🏃 RUNNING",
            "completed": "✅ COMPLETED",
            "failed": "❌ FAILED",
            "paused": "⏸️ PAUSED"
        }.get(g['status'], g['status'].upper())

        print(f"\n🎯 Big Goal: {g['title']} [{status_badge}]")
        print(f"   ID:       {g['id']}")
        print(f"   Workdir:  {g['workdir']}")
        print(f"   Schedule: {g.get('schedule') or 'Manual'}")
        print("   Subgoals:")
        for sg in g.get("subgoals", []):
            sg_icon = {
                "pending": "⏳",
                "running": "⚡",
                "passed": "✅",
                "failed": "❌",
                "blocked": "⛔",
                "skipped": "⏭️"
            }.get(sg['status'], "•")
            dyn_tag = " [DYNAMIC TRIAGE]" if sg.get("is_dynamic") else ""
            harness = sg['harness'].upper()
            print(f"     {sg_icon} [{harness}] {sg['title']}{dyn_tag} ({sg['status']})")
            if sg.get("error"):
                print(f"        └─ Error: {sg['error'][:90]}...")
            if sg.get("depends_on"):
                print(f"        └─ Prerequisite: {', '.join(sg['depends_on'])}")
    print("\n" + "=" * 70)

def cmd_tick(goal_id: str = None):
    if goal_id:
        print(f"Ticking goal `{goal_id}`...")
        res = api_post(f"/api/goals/{goal_id}/tick")
        print("Outcome:", json.dumps(res, indent=2))
    else:
        goals = api_get("/api/goals").get("goals", [])
        active = [g for g in goals if g["status"] in ("running", "queued")]
        if not active:
            print("No running or queued Big Goals to tick.")
            return

        print(f"Ticking {len(active)} active Big Goal(s)...")
        for g in active:
            print(f"\nEvaluating Goal '{g['title']}' ({g['id']})...")
            res = api_post(f"/api/goals/{g['id']}/tick")
            print("Outcome:", json.dumps(res, indent=2))

def cmd_setup_vmlab():
    workdir = "/home/ada/projects/vm-lab"
    print(f"Creating Big Goal template for vm-lab in {workdir}...")
    res = api_post("/api/goals/template/vm-lab", {
        "workdir": workdir,
        "schedule": "15m"
    })
    print("\nSuccessfully staged Big Goal:")
    print(f"  ID:       {res.get('id')}")
    print(f"  Title:    {res.get('title')}")
    print(f"  Subgoals: {len(res.get('subgoals', []))} subgoals configured:")
    for sg in res.get("subgoals", []):
        print(f"    - [{sg['harness']}] {sg['title']}")
    print("\nRun `python3 scripts/moedeck-cron.py status` to view it.")

def main():
    if len(sys.argv) < 2 or sys.argv[1] in ("-h", "--help", "help"):
        print(__doc__.strip())
        return

    cmd = sys.argv[1]
    if cmd == "status":
        cmd_status()
    elif cmd == "tick":
        gid = sys.argv[2] if len(sys.argv) > 2 else None
        cmd_tick(gid)
    elif cmd in ("setup-vmlab", "setup"):
        cmd_setup_vmlab()
    else:
        print(f"Unknown command: {cmd}")
        print(__doc__.strip())
        sys.exit(2)

if __name__ == "__main__":
    main()
