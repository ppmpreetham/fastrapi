import os

from fastrapi import FastrAPI

app = FastrAPI()


@app.get("/{name}")
def hello(name: str):
    return {"Hello": name}


if __name__ == "__main__":
    app.serve(os.getenv("BENCHMARK_HOST", "127.0.0.1"), int(os.getenv("BENCHMARK_PORT", "8000")))
