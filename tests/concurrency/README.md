# Concurrency acceptance

`workspace_stress.sh` creates named workspaces sharing one store, releases
simultaneous `svc new` processes at a barrier, and checks that every operation
and workspace root survives and reopens.

```sh
SVC_BIN="$PWD/target/debug/svc" tests/concurrency/workspace_stress.sh 32
```

The 32-client invocation is the acceptance gate. A lock timeout is a test
failure, not an allowed outcome. This covers independent workspace mutations;
the stale-root/head compare-and-swap and crash-window cases remain separate
required tests once the atomic repository transaction seam lands.
