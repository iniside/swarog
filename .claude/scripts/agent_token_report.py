"""Read-only token-usage report over Claude Code transcripts of this repo.

Usage:
  python .claude/scripts/agent_token_report.py [--days N | --since YYYY-MM-DD [--until YYYY-MM-DD]]
                                               [--projects-dir DIR] [--json]

Defaults to the last 7 days of ~/.claude/projects/<mangled repo path>. Main
transcripts are <dir>/*.jsonl, subagents <dir>/<session>/subagents/*.jsonl with a
sibling .meta.json. Context tokens = input + cache write + cache read per
assistant message (deduped by message.id). Section 2 is an estimate (chars/3.6,
images 1600 tok, each block counted once per remaining turn, reset at
compact_boundary); the residual against actual is the baseline/hidden part.
"""
import argparse
import collections
import datetime
import glob
import json
import os
import re
import statistics
import sys

NAV_TYPES = ("core-implementer", "test-author", "docs-writer", "code-scout")
SED_RE = re.compile(r"sed -n\s+'?[\d,]+p'?\s+(\S+)")


def toks(s):
    return len(s) / 3.6


def blocktoks(c):
    if isinstance(c, str):
        return toks(c)
    if isinstance(c, list):
        t = 0
        for x in c:
            if isinstance(x, dict) and x.get("type") == "image":
                t += 1600
            elif isinstance(x, dict) and x.get("type") == "text":
                t += toks(x.get("text", ""))
            else:
                t += toks(json.dumps(x))
        return t
    return toks(json.dumps(c))


def pct(vals, p):
    if not vals:
        return 0
    s = sorted(vals)
    return s[min(len(s) - 1, int(round(p * (len(s) - 1))))]


def utc_iso(dt):
    return dt.astimezone(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%S")


def parse_args():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--days", type=int, default=7)
    ap.add_argument("--since")
    ap.add_argument("--until")
    ap.add_argument("--projects-dir")
    ap.add_argument("--json", action="store_true")
    return ap.parse_args()


def resolve_dir(a):
    if a.projects_dir:
        return a.projects_dir
    repo = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
    mangled = re.sub(r"[:\\/.]", "-", repo)
    return os.path.join(os.path.expanduser("~"), ".claude", "projects", mangled)


def resolve_window(a):
    now = datetime.datetime.now().astimezone()
    if a.since:
        lo = datetime.datetime.strptime(a.since, "%Y-%m-%d").astimezone()
    else:
        lo = now - datetime.timedelta(days=a.days)
    if a.until:
        hi = datetime.datetime.strptime(a.until, "%Y-%m-%d").astimezone() + datetime.timedelta(days=1)
    else:
        hi = now + datetime.timedelta(days=1)
    return lo, hi


def classify_read(p):
    p = p.replace("\\", "/")
    if re.search(r"\.(png|jpg|jpeg|webp)$", p, re.I):
        return "image"
    if "/docs/plans/" in p:
        return "plan/checklist"
    if "/.agents/" in p or "/docs/" in p or "/.claude/" in p:
        return ".agents/docs"
    return "code/other"


def is_wide_rust_grep(i):
    p = (i.get("path") or "").replace("\\", "/").lower()
    p = re.sub(r"^[a-z]:/.*?/swarog/?", "", p).strip("/")
    g = (i.get("glob") or "") + (i.get("type") or "")
    rust = not g or "rs" in g
    depth = len([x for x in p.split("/") if x])
    return rust and depth < 2


def main():
    a = parse_args()
    root = resolve_dir(a)
    lo, hi = resolve_window(a)
    lo_s, hi_s = utc_iso(lo), utc_iso(hi)
    lo_ts = lo.timestamp()
    files = set(glob.glob(os.path.join(root, "*.jsonl")))
    files |= set(glob.glob(os.path.join(root, "*", "subagents", "*.jsonl")))
    files = sorted((f for f in files if os.stat(f).st_mtime >= lo_ts), key=os.path.getmtime)

    totals = {"main": collections.Counter(), "sub": collections.Counter()}
    per_agent = collections.defaultdict(lambda: {"n": 0, "ctx": 0, "turns": []})
    share = collections.Counter()
    actual_ctx = 0
    nav = collections.defaultdict(collections.Counter)
    first_ctx = {"main": [], "sub": []}
    atts = {}

    for f in files:
        issub = "subagents" in f.replace("\\", "/")
        kind = "sub" if issub else "main"
        at = "MAIN"
        if issub:
            at = "unknown"
            try:
                with open(f[:-6] + ".meta.json", encoding="utf-8") as mf:
                    at = json.load(mf).get("agentType") or "unknown"
            except Exception:
                pass
        live = collections.Counter()
        seen = set()
        id2 = {}
        sliced = collections.Counter()
        ts = ""
        turns = 0
        fctx = 0
        first_seen = False
        try:
            fh = open(f, encoding="utf-8", errors="replace")
        except OSError:
            continue
        with fh:
            for line in fh:
                try:
                    d = json.loads(line)
                except Exception:
                    continue
                if not isinstance(d, dict):
                    continue
                t_ = d.get("timestamp")
                if isinstance(t_, str):
                    ts = t_
                inwin = lo_s <= ts[:19] < hi_s if ts else False
                typ = d.get("type")
                if typ == "system" and d.get("subtype") == "compact_boundary":
                    live = collections.Counter()
                    continue
                if typ == "attachment":
                    att = d.get("attachment") or {}
                    t = att.get("type")
                    if t == "instructions":
                        atts[(kind, t)] = [(x.get("path", "")[-60:], len(x.get("content", ""))) for x in att.get("files", [])]
                    elif t in ("skill_listing", "agent_listing_delta", "mcp_instructions_delta", "deferred_tools_delta"):
                        atts[(kind, t)] = len(json.dumps(att))
                    continue
                m = d.get("message")
                if not isinstance(m, dict):
                    continue
                if typ == "assistant":
                    us = m.get("usage")
                    mid = m.get("id")
                    if us and mid not in seen:
                        seen.add(mid)
                        inp = us.get("input_tokens") or 0
                        cw = us.get("cache_creation_input_tokens") or 0
                        cr = us.get("cache_read_input_tokens") or 0
                        out = us.get("output_tokens") or 0
                        ctx = inp + cw + cr
                        if not first_seen:
                            first_seen = True
                            fctx = ctx
                        if inwin:
                            tc = totals[kind]
                            tc["input"] += inp
                            tc["cache_write"] += cw
                            tc["cache_read"] += cr
                            tc["output"] += out
                            turns += 1
                            actual_ctx += ctx
                            est = sum(live.values())
                            for k, v in live.items():
                                share[k] += v
                            share["_baseline/system+hidden"] += max(0, ctx - est)
                            pa = per_agent[at]
                            pa["ctx"] += ctx
                    for c in m.get("content") or []:
                        if not isinstance(c, dict):
                            continue
                        ct = c.get("type")
                        if ct == "tool_use":
                            nm = c.get("name") or "?"
                            inp_ = c.get("input") or {}
                            k = nm
                            if nm == "Read":
                                k = "Read:" + classify_read(inp_.get("file_path") or "")
                            elif nm in ("Bash", "PowerShell"):
                                cmd = re.sub(r"^(cd [^&;]+(&&|;)\s*)", "", (inp_.get("command") or "").strip())
                                w = cmd.split()
                                k = nm + ":" + (w[0] if w else "")
                                if w and w[0] in ("cargo", "git"):
                                    k = nm + ":" + " ".join(w[:2])
                            id2[c.get("id")] = k
                            live["call-input:" + nm] += toks(json.dumps(inp_))
                            if inwin and at in NAV_TYPES:
                                n = nav[at]
                                if nm == "LSP":
                                    n["LSP"] += 1
                                elif nm == "Read":
                                    n["Read"] += 1
                                elif nm == "Grep":
                                    n["Grep"] += 1
                                    if is_wide_rust_grep(inp_):
                                        n["wide_rust_grep"] += 1
                                elif nm == "Agent" and inp_.get("subagent_type") == "code-scout":
                                    n["code-scout_dispatch"] += 1
                                elif nm in ("Bash", "PowerShell"):
                                    for fn in SED_RE.findall(inp_.get("command") or ""):
                                        n["sed_slices"] += 1
                                        sliced[fn] += 1
                        elif ct == "text":
                            live["assistant text"] += toks(c.get("text", ""))
                        elif ct == "thinking":
                            live["thinking"] += toks(c.get("thinking", ""))
                elif typ == "user":
                    cont = m.get("content")
                    if isinstance(cont, str):
                        live["user/task prompt"] += toks(cont)
                    elif isinstance(cont, list):
                        for c in cont:
                            if not isinstance(c, dict):
                                continue
                            if c.get("type") == "tool_result":
                                live["result:" + id2.get(c.get("tool_use_id"), "?")] += blocktoks(c.get("content"))
                            elif c.get("type") == "text":
                                live["user/task prompt"] += toks(c.get("text", ""))
        if turns:
            pa = per_agent[at]
            pa["n"] += 1
            pa["turns"].append(turns)
            first_ctx[kind].append(fctx)
            if at in NAV_TYPES:
                for n_ in sliced.values():
                    if n_ >= 3:
                        nav[at]["files_sliced_3plus"] += 1

    est_total = sum(v for k, v in share.items() if not k.startswith("_baseline"))
    res = {
        "window": {"since": lo_s, "until": hi_s, "files": len(files), "dir": root},
        "totals": {k: dict(v) for k, v in totals.items()},
        "actual_context_tokens": actual_ctx,
        "agents": {},
        "share": {
            "estimated_tokens": est_total,
            "actual_tokens": actual_ctx,
            "residual_baseline_tokens": share.get("_baseline/system+hidden", 0),
            "by_category": dict(share.most_common()),
        },
        "navigation": {t: dict(nav[t]) for t in NAV_TYPES},
        "baseline": {
            "first_turn_context": {
                k: {"median": int(statistics.median(v)) if v else 0, "mean": int(sum(v) / len(v)) if v else 0, "files": len(v)}
                for k, v in first_ctx.items()
            },
            "attachments_latest": {f"{k[0]}:{k[1]}": v for k, v in atts.items()},
        },
    }
    for at, pa in per_agent.items():
        tr = pa["turns"]
        res["agents"][at] = {
            "dispatches": pa["n"],
            "total_context_tokens": pa["ctx"],
            "avg_context_per_dispatch": int(pa["ctx"] / pa["n"]) if pa["n"] else 0,
            "turns_p50": pct(tr, 0.5),
            "turns_p90": pct(tr, 0.9),
            "turns_max": max(tr) if tr else 0,
        }

    if a.json:
        json.dump(res, sys.stdout, indent=1)
        print()
        return
    render(res)


def fmt(n):
    n = float(n)
    for u, d in (("B", 1e9), ("M", 1e6), ("k", 1e3)):
        if abs(n) >= d:
            return f"{n / d:.2f}{u}"
    return f"{n:.0f}"


def render(r):
    w = r["window"]
    print(f"Window {w['since']} .. {w['until']} UTC, {w['files']} files in {w['dir']}\n")
    print("1. Totals (tokens)")
    print(f"  {'':10}{'input':>10}{'cache_wr':>10}{'cache_rd':>10}{'output':>10}")
    for k in ("main", "sub"):
        t = r["totals"][k]
        print(f"  {k:10}{fmt(t.get('input', 0)):>10}{fmt(t.get('cache_write', 0)):>10}{fmt(t.get('cache_read', 0)):>10}{fmt(t.get('output', 0)):>10}")
    print(f"  context total (in+cw+cr): {fmt(r['actual_context_tokens'])}\n")
    print(f"  {'agentType':20}{'disp':>6}{'total ctx':>11}{'avg/disp':>10}{'p50':>6}{'p90':>6}{'max':>6}")
    for at, v in sorted(r["agents"].items(), key=lambda x: -x[1]["total_context_tokens"]):
        print(f"  {at:20}{v['dispatches']:>6}{fmt(v['total_context_tokens']):>11}{fmt(v['avg_context_per_dispatch']):>10}{v['turns_p50']:>6}{v['turns_p90']:>6}{v['turns_max']:>6}")
    s = r["share"]
    print(f"\n2. Context share (estimated {fmt(s['estimated_tokens'])} vs actual {fmt(s['actual_tokens'])}; residual baseline/hidden {fmt(s['residual_baseline_tokens'])})")
    base = s["actual_tokens"] or 1
    for k, v in list(s["by_category"].items())[:40]:
        print(f"  {k:40}{fmt(v):>10}{100 * v / base:>7.1f}%")
    print("\n3. Navigation discipline")
    cols = ["LSP", "Grep", "wide_rust_grep", "sed_slices", "files_sliced_3plus", "code-scout_dispatch", "Read"]
    print(f"  {'agentType':20}" + "".join(f"{c[:12]:>13}" for c in cols))
    for t in NAV_TYPES:
        print(f"  {t:20}" + "".join(f"{r['navigation'][t].get(c, 0):>13}" for c in cols))
    b = r["baseline"]
    print("\n4. Baseline")
    for k, v in b["first_turn_context"].items():
        print(f"  first-turn context {k}: median {v['median']} mean {v['mean']} over {v['files']} transcripts")
    for k, v in sorted(b["attachments_latest"].items()):
        if isinstance(v, list):
            print(f"  {k}: " + ", ".join(f"{p}={n}ch" for p, n in v))
        else:
            print(f"  {k}: {v} chars")


if __name__ == "__main__":
    main()
