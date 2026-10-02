import asyncio
import os

import uvicorn
from fastapi import FastAPI

app = FastAPI()


@app.get("/")
async def sleepy():
    await asyncio.sleep(1)
    return {"ok": True}


if __name__ == "__main__":
    uvicorn.run(app, host=os.getenv("BENCHMARK_HOST", "127.0.0.1"), port=int(os.getenv("BENCHMARK_PORT", "8000")))
