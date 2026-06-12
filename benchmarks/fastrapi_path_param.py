from fastrapi import FastrAPI

app = FastrAPI()


@app.get("/{name}")
def hello(name: str):
    return {"Hello": name}


if __name__ == "__main__":
    app.serve("127.0.0.1", 8000)
