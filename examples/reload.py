from fastrapi import FastrAPI

app = FastrAPI()


@app.get("/")
def hello():
    return {"Hello": "Reload"}


app.serve("127.0.0.1", 8000, reload=True)
