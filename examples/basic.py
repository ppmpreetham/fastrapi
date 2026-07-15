from fastrapi import FastrAPI
from fastrapi.responses import JSONResponse

app = FastrAPI(redoc_url="/api-docs")

# you could try @app.get("/", cache_resp=True) if it's static and won't change :)
@app.get("/")
def hello() -> JSONResponse:
    return {"Hello": "World"}

app.serve("127.0.0.1", 8000)