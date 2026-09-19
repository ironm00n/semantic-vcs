# dsh harness

This overlay pins its runtime contract to `@deepseek-ai/dsh@0.1.5-rc.2`. It keeps the stock read, glob, and grep tools, removes direct edit and write tools per agent, disables shell and network tools, and exposes `svc` semantic operations through `svc-tools.mjs`.

Run from the repository root with an absolute `svc` binary path:

```sh
SVC_BIN="$PWD/target/debug/svc" \
OPENROUTER_API_KEY="…" \
npx -y @deepseek-ai/dsh@0.1.5-rc.2 --profile acp --patch harness/overlay.yml
```

The plugin writes nothing to stdout because stdout carries ACP frames. Harness startup diagnostics are on stderr. `session-log-deepseek` is disabled by the overlay.
