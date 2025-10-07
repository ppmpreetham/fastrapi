# FastrAPI ⚡

A blazing-fast web framework that brings Rust performance to Python developers. FastrAPI + Rust = **FastrAPI**.

## ✨ Key Features

- 🚀 **Blazing Performance**: Powered by Rust and Axum - up to **88x** faster than *FastAPI*
- 🐍 **Python-First**: Write familiar Python code with zero Rust knowledge required
- 🛡️ **Rock-Solid Types**: Built-in type safety inherited from Rust's robust type system
- 📝 **Pydantic Integration**: Full support for Pydantic models for request/response validation
- ⚡ **Async Native**: Tokio-powered async runtime for maximum concurrency
- 🪶 **Ultra Lightweight**: Minimal runtime overhead with maximum throughput
- 🎯 **Drop-in Replacement**: Compatible with FastAPI's beloved decorator syntax

## Installation

### uv
```bash
uv install fastrapi
```

### pip
```bash
pip install fastrapi
```

## Quick Start

```python
from fastrapi import FastrAPI
app = FastrAPI()

@app.get("/hello")
def hello():
    return {"Hello": "World"}

@app.post("/echo")
def echo(data):
    return {"received": data}

if __name__ == "__main__":
    app.serve("127.0.0.1", 8080)
```

### Now, test it with:
```bash
curl http://127.0.0.1:8080/hello
```

For the `POST` endpoint:
```bash
curl --location 'http://127.0.0.1:8080/echo' \
--header 'Content-Type: application/json' \
--data '{"foo": 123, "bar": [1, 2, 3]}'
```

## Performance
Benchmarks using [k6](https://k6.io/) show it outperforms FastAPI + Guvicorn across multiple worker configurations.

### 🖥️ Test Environment
- **Kernel:** 6.16.8-arch3-1  
- **CPU:** AMD Ryzen 7 7735HS (16 cores, 4.83 GHz)  
- **Memory:** 15 GB  
- **Load Test:** 20 Virtual Users (VUs), 30s  

### ⚡ Benchmark Results

| Framework                           | Avg Latency (ms) | Median Latency (ms) | Requests/sec | **Speed Metric** (req/sec ÷ avg ms) |
|------------------------------------|-----------------|-------------------|-------------|-----------------------------------|
| **FASTRAPI**                        | 2.19            | 2.16              | 8881        | **4051**                          |
| FastAPI + Uvicorn (workers: 1)     | 21.08           | 19.67             | 937         | 44                                |
| FastAPI + Uvicorn (workers: 16)    | 4.84            | 4.17              | 3882        | 802                               |


> **TLDR;** FASTRAPI handles thousands of requests per second with ultra-low latency — making it **~91× faster** than FastAPI + Guvicorn with 1 worker.

## Current Limitations
- Limited validation features compared to FastAPI's Pydantic integration
- Some advanced features are still in development

## Contributing
Contributions are welcome! Please feel free to submit a Pull Request.

- Fork the repository
- Create your feature branch (git checkout -b feature/amazing-feature)
- Commit your changes (git commit -m 'Add some amazing feature')
- Push to the branch (git push origin feature/amazing-feature)
- Open a Pull Request

## License
This project is licensed under the MIT License - see the LICENSE file for details.

## Acknowledgments
Inspired by FastAPI
Built with [PyO3](https://github.com/PyO3/pyo3/) and [Axum](https://github.com/tokio-rs/axum/)
