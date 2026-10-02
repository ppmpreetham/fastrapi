import os

from fastrapi import FastrAPI
import fastrapi.asyncio as asyncio

app = FastrAPI()


@app.get("/")
async def hello():
    return {"Hello": "World"}


if __name__ == "__main__":
    app.serve(os.getenv("BENCHMARK_HOST", "127.0.0.1"), int(os.getenv("BENCHMARK_PORT", "8000")))
