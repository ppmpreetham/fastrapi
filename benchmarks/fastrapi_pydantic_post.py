from pydantic import BaseModel

from fastrapi import FastrAPI

app = FastrAPI()


class Payload(BaseModel):
    name: str
    age: int
    active: bool = True
    score: float = 1.5


@app.post("/")
def create(payload: Payload) -> Payload:
    return payload


if __name__ == "__main__":
    app.serve("127.0.0.1", 8000)
