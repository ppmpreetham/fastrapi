# benchmarks/async bridge sanity check
from fastrapi import FastrAPI
import fastrapi.asyncio as asyncio

app = FastrAPI()

@app.get("/")
async def hello():
    return {"Hello": "World"}

app.serve("127.0.0.1", 8000)