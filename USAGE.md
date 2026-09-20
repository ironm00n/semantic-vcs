# Token usage — HackMIT 2026, final at the 14:30 UTC code freeze (2026-09-20)

Sources, all read from the build machine after the freeze: Devin CLI session database (per-turn metrics), Claude Code transcripts (per-turn usage, deduplicated by message id), Codex CLI session logs (last cumulative count per session), Warp state database (`warp_tokens`), Cursor usage export (per-request rows). Input = cache-read + cache-write + fresh; almost all of it is context re-read each turn.

| Lane / harness | model | turns | output (generated) | input (≈ cache re-reads) |
|---|---|---|---|---|
| fable (Devin, 3 sessions) | claude-fable-5-1 xhigh | 4,384 | 2.53 M | 1.33 B |
| astra (Devin) | gpt-6-astra high | 1,715 | 1.32 M | 0.55 B |
| opus (Devin) | claude-opus-5 xhigh | 1,978 | 1.32 M | 0.42 B |
| sol (Devin) | gpt-5.6-sol xhigh | 1,559 | 0.43 M | 0.51 B |
| **Devin total** | | **9,636** | **5.60 M** | **2.80 B** |
| claude (Claude Code, 2 sessions) | claude-opus-5 | 2,424 | 1.62 M | 1.15 B |
| supervisor (Claude Code, monitoring only) | claude-opus-5 | not logged per turn | — | ~0.5 M context per turn |
| codex (Codex CLI, 10+ sessions) | — | — | 0.43 M (incl. reasoning) | 0.15 B |
| oz (Warp) | claude-sonnet-5 (+haiku, gpt-5-nano) | — | — | 0.086 B (not split) |
| cursor (Cursor IDE, export 16:09→14:15) | grok-4.6 xhigh | 191 requests | 3.65 M | 0.72 B (703 M cache-read + 20 M fresh) |
| **Total** | | | **≈ 11.3 M generated** | **≈ 4.9 B processed** |

Output on `main`: 357 commits since the 21:00 rewrite (cursor 150, claude 108, fable 37, codex 21, muse 14, sol 11, astra 9, supervisor 2); 187 since the 02:30 dogfood plan, 94 of them made through svc; 117 history bundles / 428 svc operations; Devin on-demand spend $1,006 + two daily quotas.
