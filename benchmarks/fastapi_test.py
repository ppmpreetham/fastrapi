from fastapi import FastAPI
from fastapi.responses import JSONResponse

app = FastAPI()

@app.get("/")
def hello() -> JSONResponse:
    return {"Hello": "World"}

# app.serve("127.0.0.1", 8000)