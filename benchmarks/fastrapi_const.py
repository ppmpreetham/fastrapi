from fastrapi import FastrAPI
from fastrapi.responses import JSONResponse

app = FastrAPI()


# Response is computed once at startup and cached forever.
# Do not use for dynamic data, and use only for immutable responses that never change
#
# Ex:
# @app.const_get("/")
# def docs():
#     return "<html>...</html>"
#
# Not for:
# @app.const_get("/")
# def db_count():
#     return {"users": db.count()}
@app.const_get("/")
def hello():
    return {"Hello": "World"}


if __name__ == "__main__":
    app.serve("127.0.0.1", 8000)
