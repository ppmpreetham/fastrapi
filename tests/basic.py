from fastrapi import FastrAPI
from pydantic import BaseModel

app = FastrAPI()

@app.get("/")
def hello():
    return {"Hello": "World"}