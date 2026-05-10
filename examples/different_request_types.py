from fastrapi import FastrAPI
from pydantic import BaseModel

app = FastrAPI()


class EchoBody(BaseModel):
    message: str
    timestamp: int | None = None


class UpdateBody(BaseModel):
    data: dict


@app.get("/")
def hello():
    return {"Hello": "World"}

@app.get("/hello")
def hello():
    return {"Hello": "World"}

@app.get("/add")
def add():
    return {"sum": 1 + 2}

@app.post("/echo")
def echo(data: EchoBody):
    return {"received": data.model_dump()}

@app.put("/update")
def update(data: UpdateBody):
    return {"updated": data.model_dump(), "status": "success"}

@app.delete("/remove")
def remove(data: UpdateBody):
    return {"deleted": data.model_dump(), "timestamp": "2025-09-28"}

@app.patch("/modify")
def modify(data: UpdateBody):
    return {"modified": data.model_dump(), "changes": "applied"}

@app.head("/status")
def status():
    return {"alive": True}

@app.options("/info")
def info():
    return {"methods": ["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"]}

if __name__ == "__main__":
    app.serve("127.0.0.1", 8000)

