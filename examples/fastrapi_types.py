from fastrapi import FastrAPI
from fastrapi.responses import HTMLResponse, JSONResponse

api = FastrAPI()

@api.get("/html")
def get_html() -> HTMLResponse:
    return HTMLResponse("<h1>Hello</h1>")

api.serve("127.0.0.1", 8080)