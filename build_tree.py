#!/usr/bin/env python3
# build_tree.py — 离线交互网构建器
# 汇总 tasks/<任务>/runs/*/log.jsonl 的 (screen,act,diff) 三元组:
#   页面节点 = "核心骨架词"构成的身份证(核心=同页多数画面共有的短词;信息流碎片随截图轮换,自然出局)
#   边 = (起点页, 动作/按钮文字) → 实测到达的终点页(带 win/fail 计数)
# 用法: python3 build_tree.py <任务目录>   例: python3 build_tree.py tasks/今日头条遍历
# 产出: <任务目录>/tree.json + 终端打印人眼可读的网形状
import json, glob, sys, collections, os

SHORT = 6          # 骨架词长度上限(频道/标签/按钮名都短;新闻标题/作者名多数超长)
FREQ = 5           # 全局出现次数下限(一次性的信息流碎片出局)
MATCH = 0.8        # 画面归入某页的门槛: 该页核心词有 ≥80% 出现在此画面
CORE_KEEP = 0.5    # 核心收紧: 成员画面中有 ≥50% 含该词才留在核心里
EDGE_MIN = 3       # 一条边被走过这么多次才算"熟路"
EDGE_PURITY = 0.75 # 熟路的终点一致性:最常见终点占比需超过此值

def load_runs(task):
    steps = []
    for f in sorted(glob.glob(f"{task}/runs/*/log.jsonl")):
        run = os.path.basename(os.path.dirname(f))
        recs = [json.loads(l) for l in open(f) if l.strip()]
        by_n = {}
        for r in recs:
            n = r.get("n")
            if r.get("r") in ("screen", "act", "diff") and n is not None:
                by_n.setdefault(n, {})[r["r"]] = r
        for n in sorted(by_n):
            s, a, d = by_n[n].get("screen"), by_n[n].get("act"), by_n[n].get("diff")
            # 无act的步(goto断点等)不构成边材料;终局屏无act同理
            if s is not None and a is not None:
                steps.append((run, s.get("els", []), a, (d or {}).get("d", "?")))
    return steps

def main(task):
    steps = load_runs(task)
    if not steps:
        sys.exit(f"没有可用的 (screen,act,diff) 记录: {task}")

    # ── 第一遍: 全局词频筛骨架词 ──
    freq = collections.Counter()
    for _, els, *_ in steps:
        for e in els:
            t = e["t"].strip()
            if t and len(t) <= SHORT:
                freq[t] += 1
    vocab = {t for t, c in freq.items() if c >= FREQ}
    sigs = [frozenset(e["t"].strip() for e in els
                      if e["t"].strip() and len(e["t"].strip()) <= SHORT
                      and e["t"].strip() in vocab) for _, els, *_ in steps]

    # ── 第二遍: 聚类成页面。核心=成员共有的词;新画面看核心词是否 ≥80% 在场 ──
    pages = []  # {core:set, hist:Counter, members:int}
    for sig in sigs:
        best, score = None, 0.0
        for p in pages:
            c = len(p["core"] & sig) / max(len(p["core"]), 1)
            if c > score:
                best, score = p, c
        if best is not None and score >= MATCH:
            best["members"] += 1
            for t in sig:
                best["hist"][t] += 1
            best["core"] = {t for t, c in best["hist"].items()
                            if c >= best["members"] * CORE_KEEP}
        else:
            pages.append({"core": set(sig), "hist": collections.Counter(sig), "members": 1})

    # 剔除只收留了 1 个画面的"孤页"(多半是转瞬即逝的加载态/弹窗,材料不足)
    page_of = []  # 每步 → 页下标(孤页统一 -1)
    for sig in sigs:
        hit, score = -1, 0.0
        for i, p in enumerate(pages):
            if p["members"] < 2:
                continue
            c = len(p["core"] & sig) / max(len(p["core"]), 1)
            if c > score:
                hit, score = i, c
        page_of.append(hit if score >= MATCH else -1)

    # 页面命名: 核心词里挑他页少见的(TF-IDF 味道)
    def name_of(p):
        scored = sorted(p["core"], key=lambda t: -p["hist"][t] / (freq[t] + 1))
        return "/".join(scored[:4]) if scored else "(空)"

    # ── 边: 相邻步 (起点页, 动作标签) → 终点页 ──
    def act_label(els, a):
        kind = (a or {}).get("a", "?")
        if kind == "tap":
            x, y = a.get("x", -1), a.get("y", -1)
            best = None
            for e in els:
                b = e.get("b")
                if b and b[0] <= x <= b[2] and b[1] <= y <= b[3]:
                    if best is None or (b[2]-b[0])*(b[3]-b[1]) < best[0]:
                        best = ((b[2]-b[0])*(b[3]-b[1]), e["t"].strip()[:12])
            return f"点[{best[1]}]" if best else f"点裸({x},{y})"
        return {"scroll_up": "上滑", "scroll_down": "下滑", "swipe": "滑动",
                "back": "返回", "home": "回桌面", "wait": "等待"}.get(
                kind, kind if kind in ("launch", "type") else kind)

    edges = collections.defaultdict(collections.Counter)
    fails = collections.Counter()
    for i in range(len(steps) - 1):
        if steps[i][0] != steps[i+1][0] or page_of[i] < 0:
            continue
        run, els, a, d = steps[i]
        label = act_label(els, a)
        if d.startswith("rejected"):
            continue
        if d == "none" or page_of[i+1] < 0:
            fails[(page_of[i], label)] += 1
            continue
        edges[(page_of[i], label)][page_of[i+1]] += 1

    # ── 产出 ──
    pid = {i: n for n, i in enumerate(
        sorted((i for i in range(len(pages)) if pages[i]["members"] >= 2),
               key=lambda i: -pages[i]["members"]))}
    pg_out = [{"id": pid[i], "name": name_of(pages[i]),
               "core": sorted(pages[i]["core"]),
               "visits": pages[i]["members"]} for i in sorted(pid)]
    edge_out = []
    for (frm, label), tos in sorted(edges.items()):
        total = sum(tos.values())
        to, cnt = tos.most_common(1)[0]
        edge_out.append({"from": pid[frm], "label": label, "to": pid[to],
                         "n": total, "pure": round(cnt / total, 2),
                         "fail": fails[(frm, label)],
                         "ripe": total >= EDGE_MIN and cnt / total >= EDGE_PURITY})
    tree = {"v": 1, "task": task, "pages": pg_out, "edges": edge_out,
            "steps_total": len(steps), "orphan_screens": page_of.count(-1)}
    with open(f"{task}/tree.json", "w") as f:
        json.dump(tree, f, ensure_ascii=False, indent=1)

    by_id = {p["id"]: p for p in pg_out}
    print(f"共 {len(steps)} 步材料 → {len(pg_out)} 个页面, {len(edge_out)} 条边 "
          f"(熟路 {sum(1 for e in edge_out if e['ripe'])} 条, "
          f"无家可归画面 {page_of.count(-1)} 张)")
    print("\n== 页面(按到访次数) ==")
    for p in pg_out:
        print(f"  P{p['id']:>3} {p['visits']:>3}次  {p['name']}")
    print("\n== 熟路(≥{}次且终点一致≥{:.0%}) ==".format(EDGE_MIN, EDGE_PURITY))
    for e in sorted(edge_out, key=lambda e: -e["n"]):
        if e["ripe"]:
            fl = f" 败{e['fail']}" if e["fail"] else ""
            print(f"  P{e['from']:>3} --{e['label']}×{e['n']}{fl}--> P{e['to']:>3}  "
                  f"{by_id[e['from']]['name']}  ⇒  {by_id[e['to']]['name']}")

if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "tasks/今日头条遍历")
