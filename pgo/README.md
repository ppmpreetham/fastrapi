# PGO workload

`python -m pgo.train` is a local-only, bounded workload used by maturin's
`--pgo` build. It covers routing, JSON validation/serialization, dependencies,
query/path parameters, static and file responses, WebSocket upgrade/frames, and
expected error paths. It uses only the standard library as a client and a local
server; no database, network service, or third-party load generator is needed.

The release workflow enables PGO only for the native Linux x86_64 wheel. Cross
compiled targets cannot execute their instrumented wheel on the GitHub runner,
so they remain ordinary optimized builds. Windows/macOS PGO is intentionally not
enabled until native runner/toolchain coverage is established.
