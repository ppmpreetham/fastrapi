# Benchmarks

Benchmarks using [k6](https://k6.io/) show it outperforms FastAPI + Guvicorn across multiple worker configurations.

### Benchmarking Locally

For real benchmark numbers, build the PyO3 extension in release mode first:

```bash
maturin develop --release
python benchmarks/run_benchmark.py --benchmark const --framework both
```

If you benchmark a debug build, Rust-side overhead will be much higher and the numbers will be misleading.

Each benchmark scenario contains exactly three files:

```text
benchmarks/<scenario>/
├── fastapi_<scenario>.py
├── fastrapi_<scenario>.py
└── stress.js
```

Available scenarios include `const`, `async_basic`, `async_sleep`, `cache_resp`,
`path_param`, `query_param`, `pydantic_post`, `oldstyle`, `sync_threadpool`,
and `sync_uncached`. Use `--framework`, `--compare-ref`, `--host`, `--port`,
and `--startup-timeout` for additional runner controls.
