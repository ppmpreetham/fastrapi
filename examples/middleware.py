from fastrapi import FastrAPI

app = FastrAPI()

@app.middleware("http")
def auth_middleware(request):
    headers = request.get("headers", {})
    auth_header = headers.get("authorization") or headers.get("Authorization")
    if not auth_header:
        return {"error": "Unauthorized"}, 401
    return None

app.serve("127.0.0.1", 8080)