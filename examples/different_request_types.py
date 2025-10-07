from fastrapi import FastrAPI
from pydantic import BaseModel

app = FastrAPI()

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
def echo(data):
    return {"received": data}

@app.put("/update")
def update(data):
    return {"updated": data, "status": "success"}

@app.delete("/remove")
def remove(data):
    return {"deleted": data, "timestamp": "2025-09-28"}

@app.patch("/modify")
def modify(data):
    return {"modified": data, "changes": "applied"}

@app.head("/status")
def status():
    return {"alive": True}

@app.options("/info")
def info():
    return {"methods": ["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"]}

if __name__ == "__main__":
    app.serve("127.0.0.1", 8080)

