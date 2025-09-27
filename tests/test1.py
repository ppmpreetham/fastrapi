from fastrapi import FastrAPI
app = FastrAPI()

@app.get("/hello")
def hello():
    return {"message": "Hello from Rust+Python!"}

@app.get("/add")
def add():
    return {"sum": 1 + 2}

@app.post("/echo")
def echo():
    return {"echo": "This came from POST!"}

if __name__ == "__main__":
    app.serve("127.0.0.1", 8080)
