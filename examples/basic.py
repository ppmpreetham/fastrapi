from fastrapi import FastrAPI
from pydantic import BaseModel

app = FastrAPI()

@app.get("/")
def hello():
    return {"Hello": "World"}

if __name__ == "__main__":
    app.serve("127.0.0.1", 8080)