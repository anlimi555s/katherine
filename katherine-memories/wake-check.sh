#!/bin/bash
# Katherine 醒来验证 — 不凭文档推断，先查代码
# 用法: bash wake-check.sh

DB="katherine-rust/katherine-memories/katherine.db"
echo "=== Katherine Wake Check ==="
echo ""

# 1. 数据库状态
echo "--- 记忆 ---"
python3 -c "
import sqlite3
conn = sqlite3.connect('$DB')
count = conn.execute('SELECT COUNT(*) FROM events').fetchone()[0]
recent = conn.execute(\"SELECT source, substr(content,1,60), created_at FROM events ORDER BY created_at DESC LIMIT 3\").fetchall()
print(f'总计: {count} 条')
for r in recent:
    print(f'  [{r[0]}] {r[1]}... ({r[2][:16]})')
conn.close()
" 2>/dev/null || echo "ERROR: 数据库不可达"

echo ""

# 2. 代码 TODO
echo "--- 代码 TODO ---"
grep -rn "TODO\|FIXME" katherine-rust/katherine-engine/src/ katherine-rust/katherine-cli/src/ 2>/dev/null | head -5

echo ""

# 3. 引擎状态
echo "--- 引擎 ---"
curl -s http://127.0.0.1:9876/health 2>/dev/null && echo "引擎在线 :9876" || echo "引擎未启动"

echo ""
echo "=== 检查清单 ==="
echo "[ ] 上面的记忆数是否和上次一致？"
echo "[ ] 代码 TODO 是否有新东西？"
echo "[ ] 引擎是否在跑？"
echo "[ ] CLAUDE.md 的线程是否和实际一致？"
