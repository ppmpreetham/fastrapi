from fastrapi import FastrAPI

app = FastrAPI()


@app.get("/", cache_resp=True)
def hello():
    return {"Hello": "World"}


if __name__ == "__main__":
    app.serve("127.0.0.1", 8000)
