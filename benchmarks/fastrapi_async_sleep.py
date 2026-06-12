import asyncio

from fastrapi import FastrAPI

app = FastrAPI()


@app.get("/")
async def sleepy():
    await asyncio.sleep(1)
    return {"ok": True}


app.serve("127.0.0.1", 8000)
