"""Katherine 醒来验证 — 不凭文档推断，先查代码"""
import sqlite3, os, subprocess, sys

DB = os.path.join(os.path.dirname(__file__), "katherine.db")

print("=== Katherine Wake Check ===\n")

# 1. 数据库
print("--- 记忆 ---")
try:
    conn = sqlite3.connect(DB)
    count = conn.execute("SELECT COUNT(*) FROM events").fetchone()[0]
    sources = conn.execute(
        "SELECT source, COUNT(*) FROM events GROUP BY source ORDER BY COUNT(*) DESC"
    ).fetchall()
    recent = conn.execute(
        "SELECT source, substr(content,1,80), created_at FROM events ORDER BY created_at DESC LIMIT 3"
    ).fetchall()
    print(f"总计: {count} 条")
    print(f"来源分布: {dict(sources)}")
    for r in recent:
        print(f"  [{r[0]}] {r[1]}... ({r[2][:16]})")
    conn.close()
except Exception as e:
    print(f"ERROR: {e}")

print()

# 2. 代码 TODO
print("--- 代码 TODO ---")
root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
for sub in ["katherine-engine/src", "katherine-cli/src"]:
    for dirpath, _, filenames in os.walk(os.path.join(root, sub)):
        for f in filenames:
            if f.endswith(".rs"):
                path = os.path.join(dirpath, f)
                with open(path, errors="ignore") as fh:
                    for i, line in enumerate(fh, 1):
                        if "TODO" in line or "FIXME" in line:
                            print(f"  {os.path.relpath(path, root)}:{i}: {line.strip()[:100]}")

print()

# 3. 引擎
print("--- 引擎 ---")
try:
    import urllib.request
    resp = urllib.request.urlopen("http://127.0.0.1:9876/health", timeout=3)
    print(f"引擎在线 :9876 ({resp.read().decode()[:100]})")
except:
    print("引擎未启动")

print()
print("=== 检查清单 ===")
print("[ ] 记忆数是否和上次一致？")
print("[ ] 代码 TODO 是否有新增？")
print("[ ] 是否先查了这个再读 CLAUDE.md？")
