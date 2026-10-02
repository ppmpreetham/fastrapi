import os

import uvicorn
from fastapi import FastAPI

app = FastAPI()


@app.get("/{name}")
def hello(name: str):
    return {"Hello": name}


if __name__ == "__main__":
    uvicorn.run(app, host=os.getenv("BENCHMARK_HOST", "127.0.0.1"), port=int(os.getenv("BENCHMARK_PORT", "8000")))
