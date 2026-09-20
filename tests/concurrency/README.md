# Concurrency acceptance

`workspace_stress.sh` creates named workspaces sharing one store, releases
simultaneous `svc new` processes at a barrier, and checks that every operation
and workspace root survives and reopens.

```sh
SVC_BIN="$PWD/target/debug/svc" tests/concurrency/workspace_stress.sh 32
```

The 32-client invocation is the acceptance gate (`demo/run.sh` runs it). A lock
timeout is a test failure, not an allowed outcome: the store is shared between
processes, so nothing waits on it. What a process can wait on is its own
checkout (`checkout busy`, bounded by `SVC_LOCK_TIMEOUT_MS`) — `demo/store-stress.sh`
covers that, and `crates/svc-repo/tests/concurrency.rs` covers the race this
script cannot stage: two processes computing on one head, where exactly one
publishes and the other is refused with nothing written.
