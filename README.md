# FastrAPI

FastrAPI is a high-performance web framework for building APIs with Python and Rust. It leverages the speed of Rust while providing a user-friendly Python interface.

## Features

- **Lightning Fast**: Built with Rust and Axum for exceptional performance
- **Familiar Syntax**: Mimics FastAPI's intuitive decorator syntax
- **Type Safety**: Leverages Rust's strong type system
- **Async First**: Built on top of Tokio for efficient async handling
- **Lightweight**: Minimal overhead compared to pure Python frameworks

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
    return {"message": "Hello from Rust+Python!"}

@app.get("/add")
def add():
    return {"sum": 1 + 2}
```

## Performance
FastrAPI provides significant performance improvements over traditional Python web frameworks:
<!-- Table -->
| Framework | Requests/sec | Latency (ms) |
|-----------|--------------|---------------|
| FastrAPI  | ~30,000      | ~6.2         |
| FastAPI   | ~8,000       | ~12.0       |
| Flask     | ~5,000       | ~18.5       |

## Why FastrAPI?
FastrAPI combines the developer-friendly syntax of FastAPI with the raw performance of Rust. It's designed for developers who:

- Need the highest possible performance for their APIs
- Love FastAPI's developer experience
- Want to leverage Rust's speed without writing Rust code

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
Built with PyO3 and Axum
