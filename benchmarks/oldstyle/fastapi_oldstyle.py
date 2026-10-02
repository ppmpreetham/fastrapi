import os

import uvicorn
from fastapi import Body, FastAPI
from pydantic import BaseModel

app = FastAPI()


class Payload(BaseModel):
    name: str
    age: int
    active: bool = True
    score: float = 1.5


@app.post("/")
def create(payload: dict = Body(...)):
    return Payload.model_validate(payload).model_dump()


if __name__ == "__main__":
    uvicorn.run(app, host=os.getenv("BENCHMARK_HOST", "127.0.0.1"), port=int(os.getenv("BENCHMARK_PORT", "8000")))
