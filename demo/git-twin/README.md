# Git twin

`build.sh` creates the two demo branches from `demo/config`, merges them with ordinary Git, compiles the result, and proves that `log(&raw)` now appears after the newly shadowing `raw` binding. Git reports a clean merge even though the logging branch intended the original input.

The generated repository is `work/` and is intentionally ignored. Remove it before rebuilding.
