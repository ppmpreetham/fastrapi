from fastrapi import FastrAPI

app = FastrAPI()


@app.get("/")
def hello(name: str = "World"):
    return {"Hello": name}


if __name__ == "__main__":
    app.serve("127.0.0.1", 8000)
