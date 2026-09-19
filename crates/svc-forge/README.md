# svc forge

A read-only localhost forge for semantic repositories. It browses repository
heads, snapshots, typed entities, typed operation history, merge conflicts, and
the review queue. It intentionally reads a JSON catalog rather than opening the
live redb database, so it cannot contend with or corrupt an active `svc` writer.

Run it from a semantic repository:

```sh
cargo run -p svc-forge -- --catalog .svc/forge.json
```

Then open <http://127.0.0.1:7742>. The server rejects non-loopback binds unless
`--allow-remote` is explicit.

Pass `--catalog` repeatedly to serve several repositories. Repository slugs
must be unique across the inputs. Catalogs are reopened per request, so an
atomic `svc forge export` replacement becomes visible without restarting the
server.

The catalog schema is the serde shape of `svc_forge::Catalog`; operations,
conflicts and reviews are the canonical `svc-core` types. See
`examples/forge.json` for a minimal catalog suitable for a smoke test.
