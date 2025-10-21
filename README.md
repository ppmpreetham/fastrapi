# FastrAPI (Fast + Rust + API)

<img src="https://raw.githubusercontent.com/ppmpreetham/fastrapi/refs/heads/main/readme/fastrapi.gif" width="100%" alt="FastRAPI GIF">
FastrAPI is a high-performance web framework that supercharges your Python APIs with the power of Rust. Built on Axum and PyO3, it delivers unmatched speed, type safety, and developer-friendly Python syntax. Create robust, async-ready APIs with minimal overhead and maximum throughput. FastrAPI is your drop-in replacement for FastAPI, offering familiar syntax with up to 31x faster performance.

## Key Features

- **Lightning Speed**: Powered by Rust and Axum, FastrAPI delivers up to 31x faster performance than FastAPI, making your APIs scream.
- **Python-First**: Write clean, familiar Python code—no Rust knowledge needed. FastrAPI handles the heavy lifting behind the scenes.
- **Ironclad Type Safety**: Inherits Rust's robust type system for rock-solid reliability, catching errors before they hit production.
- **Pydantic Powered**: Seamless integration with Pydantic for effortless request and response validation, keeping your data in check.
- **Async Native**: Built on Tokio's async runtime, FastrAPI maximizes concurrency for handling thousands of requests with ease.
- **Ultra Lightweight**: Minimal runtime overhead with maximum throughput.
- **Drop-in Replacement**: Drop-in compatibility with FastAPI's beloved decorator syntax, so you can switch without rewriting your codebase.

---

#### Is it as fast as claimed?
Yes. Powered by Rust and Axum, FastrAPI outperforms FastAPI by up to 31x in real-world benchmarks, with no compromises on usability. Check it out [here](https://github.com/ppmpreetham/fastrapi?tab=readme-ov-file#performance)

#### Do I need to know Rust?
Nope. FastrAPI lets you write 100% Python code while leveraging Rust's performance under the hood.

#### Can it handle complex APIs?
Absolutely. With full Pydantic integration and async support, FastrAPI scales effortlessly for small projects and enterprise-grade APIs alike.

#### Will it keep up with FastAPI updates?
Yes. FastrAPI mirrors FastAPI's decorator-based syntax, ensuring compatibility and instant access to familiar workflows.

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


<details>
  <summary>Show Pydantic example</summary>

```python
from pydantic import BaseModel
from fastrapi import FastrAPI

api = FastrAPI()

class User(BaseModel):
    name: str
    age: int

@api.post("/create_user")
def create_user(data: User):
    return {"msg": f"Hello {data.name}, age {data.age}"}

api.serve("127.0.0.1", 8080)
```

</details>

## Performance
Benchmarks using [k6](https://k6.io/) show it outperforms FastAPI + Guvicorn across multiple worker configurations.

### 🖥️ Test Environment
- **Kernel:** 6.16.8-arch3-1  
- **CPU:** AMD Ryzen 7 7735HS (16 cores, 4.83 GHz)  
- **Memory:** 15 GB  
- **Load Test:** 20 Virtual Users (VUs), 30s  

### ⚡ Benchmark Results

| Framework                              | Avg Latency (ms) | Median Latency (ms) | Requests/sec | P95 Latency (ms) | P99 Latency (ms) |
|----------------------------------------|------------------|---------------------|---------------|------------------|------------------|
| **FASTRAPI**                           | **0.63**         | **0.00**            | **29273**     | **2.38**         | **12.22**        |
| FastAPI + Guvicorn (workers: 1)       | 21.08            | 19.67               | 937           | 38.47            | 93.42            |
| FastAPI + Guvicorn (workers: 16)      | 4.84             | 4.17                | 3882          | 10.22            | 81.20            |

> **TLDR;** FASTRAPI handles thousands of requests per second with ultra-low latency — making it **~31× faster** than FastAPI + Guvicorn with 1 worker.

## Current Limitations
Some advanced features are still in development like:

- [ ] Middleware
- [ ] OpenAPI docs generation
- [ ] Websockets
- [ ] Dependency injection
- [ ] Better error handling (currently shows Rust errors)
- [ ] Background tasks
- [ ] Static file serving
- [ ] Testing support
- [ ] Logging/metrics (maybe next)
- [ ] Rate limiter (even FastAPI doesn't have it)
- [ ] GraphQL support
- [ ] A nice logging tool

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
