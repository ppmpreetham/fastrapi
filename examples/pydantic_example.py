from pydantic import BaseModel
from fastrapi import FastrAPI

api = FastrAPI()

class User(BaseModel):
    name: str
    age: int

class Address(BaseModel):
    street: str
    city: str
    zip: str

@api.post("/register")
def register(user: User, address: Address):
    return {
        "msg": f"Registered {user.name}, age {user.age}, living at {address.street}, {address.city} {address.zip}"
    }

if __name__ == "__main__":
    api.serve("127.0.0.1", 8080)