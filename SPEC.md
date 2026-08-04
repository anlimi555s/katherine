# Katherine v3 — 实施规格

> **权威设计参考**：[桌面设计稿](C:\Users\Selena\Desktop\Katherine-v3-设计稿.md)
> 本文件是实施配套——模块结构、文件名、具体 SQL。
> 基于 30+ 篇论文，2026-08-04

## 核心原则

### P0 — 不可变原文日志

**来源**："The Price of Meaning" (2603.27116) Theorem 7——在任何核阈值语义记忆系统中，遗忘和假召回在数学上不可避免。唯一的逃脱路径：**外挂一个精确的情景记录。** 原文永远不被 LLM 修改、总结、压缩。

### P1 — 决策边界，不是描述相似度

**来源**：DeMem (2605.10870)——描述相似度是证据兼容性的极弱预测器（ρ=0.103, AUC=0.548）。什么该被检索不由"多相关"决定，由"会不会导致不同决策"决定。CAMeR (2607.20458)——关键词门控是廉价决策代理：一个词级 Jaccard 门控比嵌入余弦高 1.6× 区分度。

### P2 — 可逆、查询条件的压缩

**来源**：R-D Compaction Survey (2607.08032)——所有四层压缩的致命失败模式相同：**在查询未知前、不可逆地丢弃信息。** 所有衰减/归档/压缩是检索时的决策，不是存储时的。

### P3 — 混合检索 + 符号门控

**来源**：MemX (2603.16171) + CAMeR + BEIR 2026 leaderboard
- BM25/FTS5：精确关键词，零嵌入依赖
- 稠密向量：语义召回，本地 ONNX (bge-small, 32MB)
- RRF 融合：零配置，不依赖分数尺度
- 关键词门控：拒绝假阳性（"pet python" vs "Python programming"）

### P4 — 分源衰减，幂律为主

**来源**：FadeMem (2601.18642) + "The Price of Meaning"——幂律衰减不是工程选择，是语义记忆在幂律到达率下的数学必然后果。
- selena_correction → 幂律，永不完全遗忘
- engine_log → 指数，快速衰减
- insight → 对数，缓慢衰减
- identity_anchor → 不衰减

## 存储：libSQL 单文件

```
katherine-memory.db  (单文件，零外部服务)
├── events            ← 不可变原文日志
├── fts_events        ← FTS5 全文索引 (BM25)
├── vectors           ← libSQL vector 扩展 (DiskANN)
├── edges             ← 7 种关系边
└── state             ← 运行时状态
```

### events 表

| 列 | 类型 | 说明 |
|----|------|------|
| id | TEXT | BLAKE3 内容哈希 |
| content | TEXT | [Selena]/[Katherine] 原文，永不变 |
| event_type | TEXT | correction / decision / insight / engine / dialogue |
| importance | REAL | 0.0-1.0，来源标签决定初始值 |
| decay_curve | TEXT | power_law / exponential / logarithmic / none |
| created_at | TEXT | ISO 时间戳 |
| last_retrieved | TEXT | 最后被检索时间 |
| retrieval_count | INT | 被检索次数（非访问次数） |
| decision_score | REAL | 决策关键度，Neuro 信号更新 |

### edges 表

| 类型 | 含义 |
|------|------|
| supersedes | 新纠正替代旧记忆 |
| contradicts | 矛盾（两版都活着，待裁决） |
| supports | 补充证据 |
| extends | 细化/扩展 |
| relates_to | 弱关联 |
| temporal_next | 时序先后 |
| caused_by | 因果链 |

## 检索管道

### v3.0：FTS5 BM25 + 四因子重排

```
Query
  │
  ├─→ FTS5 BM25 召回 (top-50)
  │     SELECT e.*, fts.rank FROM fts_events fts
  │     JOIN events e ON fts.rowid = e.rowid
  │     WHERE fts.content MATCH ?
  │     ORDER BY rank LIMIT 50
  │
  ├─→ (FTS5 空结果时) Jaccard 兜底
  │     分词 → 对所有 events 算 Jaccard → 取 top-50
  │     和现在 local_store.rs::search_sync() 一样
  │
  ▼
四因子重排 (Rust)
  │
  │  对每个候选计算：
  │
  │  recency = match decay_curve {
  │    power_law    → (1 + days/30.0)^(-1.5)
  │    exponential  → e^(-0.05 × days)
  │    logarithmic  → 1 / ln(e + days/7)
  │    none         → 1.0
  │  }
  │  frequency = min(1.0, ln(retrieval_count + 1) / 10)
  │
  │  composite = 0.45·bm25_norm + 0.25·recency + 0.05·frequency + 0.10·importance
  │
  ▼
置信拒绝
  │  max(composite) < 0.50 → 返回空
  │
  ▼
top-k → 注入上下文
```

### SQL 和 Rust 的分工

| 步骤 | 在哪 | 说明 |
|------|------|------|
| FTS5 BM25 召回 | SQL | SQLite FTS5 内置，一条 `MATCH` 查询 |
| Jaccard 兜底 | Rust | FTS5 空结果时，分词后遍历 events 表 |
| 四因子计算 | Rust | days_since、decay_curve 分支、频率对数 |
| 归一化 + 排序 | Rust | z-score 归一化后 sigmoid 压缩 |
| 置信拒绝 | Rust | max_score < τ → ∅ |
| 结果注入 | Rust | top-k 格式化后注入 system prompt |

### v3.1：加向量

```
Query
  │
  ├─→ FTS5 BM25 (top-50) ──────────┐
  ├─→ Vector cosine (top-50) ───────┤
  │                                  │
  │                       ┌──────────▼──────────┐
  │                       │   Keyword Gate       │
  │                       │   Jaccard(query, m)  │
  │                       │   拒绝假阳性          │
  │                       │   (来自 CAMeR)       │
  │                       └──────────┬──────────┘
  │                                  │
  │                       ┌──────────▼──────────┐
  │                       │   RRF 融合           │
  │                       │   合并两个排序列表    │
  │                       └──────────┬──────────┘
  │                                  │
  │                       ┌──────────▼──────────┐
  │                       │   四因子重排          │
  │                       │   (同 v3.0)          │
  │                       └──────────┬──────────┘
  │                                  │
  │                           置信拒绝 → top-k
```

向量嵌入方式：写入时调 OpenAI-compatible API（DeepSeek embedding），存 `vectors` 表。检索时同样调 API 获取查询向量，在 Rust 里做 brute-force cosine（<10K 条够快）。后续换 ONNX 本地模型——接口不变，只是嵌入源从 API 变成本地文件。

### 检索在两种环境中的调用

| 环境 | 方式 |
|------|------|
| Rust 引擎 loop | `LibSqlHub::recall(query, limit)` → 完整管道 |
| Claude Code（我） | `sqlite3 katherine.db "SELECT * FROM fts_events WHERE content MATCH '...'"` — 只用 BM25，不需要四因子（我在上下文中自己判断） |

## 衰减曲线

```
correction:    R(t) = (1 + t/30)^(-1.5)   幂律，半衰约30天，永不归零
decision:      R(t) = (1 + t/60)^(-1.0)   幂律，半衰约60天
insight:       R(t) = 1 / log(e + t/7)    对数，极缓慢
engine_log:    R(t) = e^(-0.05·t)         指数，约14天半衰
dialogue:      R(t) = e^(-0.02·t)         指数，约35天半衰
identity:      R(t) = 1.0                 不衰减
```

## Neuro v3 — 独立观察者

**来源**：VIGIL (2512.07094) + AgentTrace (2602.10133) + Cognitive Companion (2604.13759) + Springdrift (2604.04660)

### 架构：同伴观察者模式

```
loop_.rs  ──emit Event──▶  Neuro  ──persist──▶  katherine.db
  (主循环)                  (独立观察者)
```

Neuro 不再是被动的 `set_pressure()` 接收方。它从主循环接收结构化事件，独立处理。主循环不做自我诊断——Neuro 做。大部分是确定性代码（衰减、阈值、状态转移），LLM 只用于诊断报告生成。

### 三层日志

| 层 | 内容 | 当前状态 |
|----|------|---------|
| 操作层 | 轮次/Token/工具调用/失败/API耗时/错误环 | ✅ 已有 |
| 认知层 | 推理新颖性、主题相关性、步骤冗余度 | ❌ 新增 |
| 上下文层 | 退化发生时对话在做什么、触发条件 | ❌ 新增 |

### 持久化

每次会话结束时 dump Neuro 快照到 `neuro_snapshots` 表。启动时加载上一个快照。重启不丢错误计数、工具失败模式、认知状态趋势。

### 结构化诊断（EmoBank 模式）

不存储原始指标，存储结构化诊断：
- **Roses**：稳定成功（某工具始终正确、某模式重复生效）
- **Buds**：新兴机会（新工具被正确使用、新策略初见成效）
- **Thorns**：系统故障（同一工具连续失败、循环检测触发、主题漂移）

诊断报告在自检时生成，LLM 只用于报告文案。

### Neuro ↔ 记忆交互

Neuro 信号在检索时注入，不修改存储：
- 重复检测 → 同类记忆的 decision_score 临时降低
- 压力 > 80% → 检索限 top-3，减少上下文注入
- 错误计数升高 → 最近纠正的 retrieval 优先级提升
- Roses 模式 → 相关成功的工具/策略记忆获得检索 boost

## 迁移路径

1. libSQL schema 建好
2. ChromaDB 123 条 → `events` 表（一次性导入）
3. LocalMemoryStore memory.json → 删除
4. Hub :8765 `/memory` 端点 → 改为直连 libSQL
5. Rust 引擎 `Hub trait` → 实现 libSQL 后端

## 参考论文

| 论文 | 用在哪 |
|------|--------|
| DeMem (2605.10870) | P1 决策边界 |
| Price of Meaning (2603.27116) | P0 不可变原文 + P4 幂律证明 |
| R-D Compaction Survey (2607.08032) | P2 可逆压缩 |
| CAMeR (2607.20458) | P3 关键词门控 |
| FadeMem (2601.18642) | P4 分源衰减 |
| MemX (2603.16171) | P3 检索管道参考 |
| MemStrata (2606.26511) | 确定性替代规则 |
| STALE (2605.06527) | 醒来验证流程 |
| TrustMem (2606.25161) | 记忆写入验证（留给人检） |
| Intelligent Decay (2509.25250) | 综合效用分 |
| Cortex/MAG/AgenticMemory | 工程可行性证明 |
