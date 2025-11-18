from fastrapi import FastrAPI
from fastrapi.responses import JSONResponse

from fastrapi.middleware import (
    CORSMiddleware,
    TrustedHostMiddleware,
    GZipMiddleware,
    SessionMiddleware
)

app = FastrAPI()

# TrustedHost Middleware
app.add_middleware(
    TrustedHostMiddleware, 
    allowed_hosts=["127.0.0.1", "localhost", "127.0.0.1:8000"],
    www_redirect=True
)

# CORS Middleware
app.add_middleware(
    CORSMiddleware,
    allow_origins=["*"],
    allow_methods=["GET", "POST"],
    allow_headers=["*"],
    allow_credentials=False
)

# 3. GZip Middleware
app.add_middleware(
    GZipMiddleware, 
    minimum_size=500,
    compresslevel=9
)

# 4. Session Middleware
app.add_middleware(
    SessionMiddleware,
    secret_key="super-duper-secret-key-change-this-in-prod-pwease-uwu-BUT-MAKE-IT-LONGER-NOW",
    session_cookie="fastrapi_session",
    max_age=3600,
    https_only=False
)

# ROUTES
# WARNING: ALWAYS return JSONResponse if it's JSON, to ensure proper serialization
@app.get("/")
def index() -> JSONResponse:
    return JSONResponse({"status": "running"})

@app.get("/heavy")
def heavy_data() -> JSONResponse:
    # response large enough to trigger GZip compression
    large_data = "x" * 1000
    return JSONResponse({
        "data": large_data,
        "note": "Check content-encoding header!"
    })

# Session Test: Increment a counter stored in the cookie
@app.get("/counter")
def session_counter(request) -> JSONResponse:
    # For now, this verifies the Middleware sets the cookie correctly.
    return JSONResponse({"message": "Session cookie should be set"})

if __name__ == "__main__":
    app.serve("127.0.0.1", 8000)