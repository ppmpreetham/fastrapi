from fastrapi import FastrAPI
from fastrapi.responses import JSONResponse

app = FastrAPI()

@app.get("/")
def hello() -> JSONResponse:
    return {"Hello": "World"}

if __name__ == "__main__":
    app.serve("127.0.0.1", 8000)