from fastrapi import FastrAPI
from fastrapi.responses import PlainTextResponse

app = FastrAPI()


@app.middleware("http")
def empty_response(request):
    if request["path"] == "/empty":
        return PlainTextResponse("", status_code=204)
    return None


@app.get("/")
def hello():
    return {"Hello": "World"}


@app.get("/empty")
def should_not_run():
    return {"wrong": True}


if __name__ == "__main__":
    app.serve("127.0.0.1", 8000)
