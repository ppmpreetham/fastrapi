from fastrapi import FastrAPI, StaticFiles

app = FastrAPI()
app.mount("/static", StaticFiles(directory="static", html=True), name="static")


@app.get("/")
def index():
    return {"static": "/static"}


if __name__ == "__main__":
    app.serve("127.0.0.1", 8000)
