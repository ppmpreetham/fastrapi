from typing import List, Optional

from fastrapi import (
    Cookie,
    Depends,
    FastrAPI,
    Header,
    HTTPException,
    Query,
    Security,
    SecurityScopes,
)
from fastrapi import (
    Path as PathParam,
)
from fastrapi.middleware import (
    CORSMiddleware,
    GZipMiddleware,
    SessionMiddleware,
    TrustedHostMiddleware,
)
from fastrapi.responses import (
    HTMLResponse,
    JSONResponse,
)
from pydantic import BaseModel

app = FastrAPI(
    title="FastrAPI PGO Workload",
    version="0.3.0",
    description="Broad local workload used by maturin PGO training.",
    openapi_url="/api-docs/openapi.json",
)

app.add_middleware(
    TrustedHostMiddleware,
    allowed_hosts=["127.0.0.1", "localhost", "127.0.0.1:8000"],
    www_redirect=True,
)
app.add_middleware(
    CORSMiddleware,
    allow_origins=["*"],
    allow_methods=["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"],
    allow_headers=["*"],
    allow_credentials=False,
)
app.add_middleware(GZipMiddleware, minimum_size=500, compresslevel=6)
app.add_middleware(
    SessionMiddleware,
    secret_key="pgo-local-training-secret-change-in-real-apps-64-bytes-minimum-for-cookie-key",
    session_cookie="fastrapi_session",
    max_age=3600,
    https_only=False,
)


class Address(BaseModel):
    line1: str
    city: str
    region: str
    postal_code: str
    country: str


class User(BaseModel):
    id: Optional[int] = None
    name: str
    age: int
    email: str
    roles: List[str] = []
    address: Optional[Address] = None


class Login(BaseModel):
    username: str
    password: str
    remember: bool = False


class Product(BaseModel):
    sku: str
    name: str
    price: float
    currency: str = "USD"
    tags: List[str] = []
    metadata: dict = {}


class OrderLine(BaseModel):
    sku: str
    quantity: int
    unit_price: float


class Order(BaseModel):
    order_id: str
    customer_id: int
    status: str
    lines: List[OrderLine]
    shipping_address: Address
    notes: Optional[str] = None


class Event(BaseModel):
    event_id: str
    name: str
    source: str
    timestamp: str
    properties: dict = {}


class EventBatch(BaseModel):
    batch_id: str
    events: List[Event]


class GenericJSON(BaseModel):
    data: dict


class SupportTicket(BaseModel):
    subject: str
    priority: str
    requester: User
    message: str
    tags: List[str] = []


USERS = {
    1: {"id": 1, "name": "Preetham", "email": "preetham@example.com"},
    2: {"id": 2, "name": "Alice Chen", "email": "alice@example.com"},
    42: {"id": 42, "name": "Jordan Lee", "email": "jordan@example.com"},
}

PRODUCTS = {
    "sku-1001": {"sku": "sku-1001", "name": "API Gateway", "price": 129.0},
    "sku-2002": {"sku": "sku-2002", "name": "Observability Pack", "price": 89.5},
}


def current_user():
    return {"user_id": 42, "role": "admin", "tenant": "tenant-prod-17"}


def verify_token(token: str = Header(default=None)):
    if token not in {"valid-token", "admin-token"}:
        raise HTTPException(status_code=401, detail="Invalid token")
    return token


def verify_scopes(security_scopes: SecurityScopes, token: str = Header(default=None)):
    if token != "admin-token":
        raise HTTPException(status_code=401, detail="No admin token")
    return {"token": token, "scopes": security_scopes.scopes}


@app.middleware("http")
def pgo_trace_middleware(request):
    headers = request.get("headers", {})
    if headers.get("x-pgo-block") == "1":
        return {"error": "blocked by pgo middleware"}, 403
    return None


@app.get("/")
def index() -> JSONResponse:
    return JSONResponse({"service": "fastrapi-pgo", "status": "running"})


@app.get("/health")
def health():
    return {"ok": True, "checks": {"app": "up", "payloads": "ready"}}


@app.get("/status")
def status():
    return {"alive": True, "workers": 4, "queue_depth": 0}


@app.head("/status")
def status_head():
    return {"alive": True}


@app.options("/status")
def status_options():
    return {"methods": ["GET", "HEAD", "OPTIONS"]}


@app.get("/config")
def config():
    return {
        "environment": "pgo",
        "features": ["routing", "validation", "middleware", "openapi"],
        "limits": {"max_page_size": 100, "json_body_mb": 8},
    }


@app.get("/version")
def version():
    return {"name": "fastrapi", "version": "0.3.0", "profile": "pgo"}


@app.get("/html")
def html() -> HTMLResponse:
    return HTMLResponse(
        """
        <main>
          <h1>FastrAPI PGO</h1>
          <p>HTML response path for workload training.</p>
        </main>
        """
    )


@app.get("/heavy")
def heavy() -> JSONResponse:
    return JSONResponse(
        {
            "kind": "large-response",
            "items": [
                {
                    "id": index,
                    "name": f"record-{index}",
                    "description": "x" * 256,
                    "tags": ["pgo", "gzip", "json"],
                }
                for index in range(80)
            ],
        }
    )


@app.get("/users/{user_id}")
def get_user(user_id: int):
    user = USERS.get(user_id)
    if user is None:
        raise HTTPException(status_code=404, detail="User not found")
    return {"user": user}


@app.get("/users/{user_id}/orders/{order_id}")
def get_user_order(user_id: int, order_id: str):
    return {"user_id": user_id, "order_id": order_id, "status": "processing"}


@app.post("/users")
def create_user(user: User):
    return {"created": user.model_dump(), "normalized_email": user.email.lower()}


@app.post("/register")
def register(user: User, address: Address):
    return {
        "user": user.model_dump(),
        "address": address.model_dump(),
        "message": f"Registered {user.name} in {address.city}",
    }


@app.patch("/users/{user_id}")
def update_user(user_id: int, body: GenericJSON):
    return {"user_id": user_id, "patch": body.data, "updated": True}


@app.post("/auth/login")
def login(credentials: Login):
    return {
        "authenticated": True,
        "user": credentials.username,
        "remember": credentials.remember,
        "token_type": "bearer",
    }


@app.get("/protected")
def protected(user=Depends(current_user)):
    return {"message": "protected", "user": user}


@app.get("/dependency-user")
def dependency_user(user=Depends(current_user)):
    return {"message": "dependency route", "user": user}


@app.get("/auth/header")
def auth_header(token=Depends(verify_token)):
    return {"authenticated": True, "token": token}


@app.post("/auth/refresh")
def refresh_token(body: GenericJSON, token=Depends(verify_token)):
    return {"refreshed": True, "token": token, "claims": body.data}


@app.get("/admin")
def admin_only(auth=Security(verify_scopes, scopes=["admin"])):
    return {"message": "admin access", "auth": auth}


@app.post("/admin/audit")
def admin_audit(
    body: GenericJSON, auth=Security(verify_scopes, scopes=["admin", "audit"])
):
    return {"accepted": True, "auth": auth, "event": body.data}


@app.get("/session")
def session_cookie(fastrapi_session: str = Cookie(default="missing")):
    return {"session": fastrapi_session}


@app.get("/products/{sku}")
def get_product(sku: str):
    return {"product": PRODUCTS.get(sku, {"sku": sku, "name": "Unknown", "price": 0.0})}


@app.post("/products")
def create_product(product: Product):
    return {"product": product.model_dump(), "created": True}


@app.put("/products/{sku}")
def replace_product(sku: str, product: Product):
    data = product.model_dump()
    data["sku"] = sku
    return {"product": data, "replaced": True}


@app.get("/search")
def search(q: str = Query(""), limit: int = Query(10), offset: int = Query(0)):
    return {
        "query": q,
        "limit": limit,
        "offset": offset,
        "results": [
            {"id": offset + item, "title": f"{q or 'item'}-{offset + item}"}
            for item in range(min(limit, 10))
        ],
    }


@app.get("/validate/query")
def validate_query(
    age: int = Query(default=18, ge=0, le=150),
    name: str = Query(default="Anonymous", min_length=1, max_length=50),
):
    return {"age": age, "name": name}


@app.get("/validate/path/{user_id}")
def validate_path(user_id: int = PathParam(..., ge=1, le=1000)):
    return {"user_id": user_id}


@app.get("/orders/{order_id}")
def get_order(order_id: str):
    return {"order_id": order_id, "status": "processing", "total": 248.5}


@app.post("/orders")
def create_order(order: Order):
    return {
        "accepted": True,
        "order_id": order.order_id,
        "line_count": len(order.lines),
        "total": sum(line.quantity * line.unit_price for line in order.lines),
    }


@app.put("/orders/{order_id}")
def replace_order(order_id: str, order: Order):
    data = order.model_dump()
    data["order_id"] = order_id
    return {"order": data, "replaced": True}


@app.patch("/orders/{order_id}")
def patch_order(order_id: str, body: GenericJSON):
    return {"order_id": order_id, "patch": body.data, "updated": True}


@app.delete("/orders/{order_id}")
def cancel_order(order_id: str):
    return {"order_id": order_id, "cancelled": True}


@app.get("/inventory/{sku}")
def get_inventory(sku: str):
    return {"sku": sku, "available": 128, "reserved": 7, "warehouse": "iad-1"}


@app.post("/inventory/bulk")
def bulk_inventory(body: GenericJSON):
    updates = body.data.get("updates", [])
    return {"accepted": len(updates), "status": "queued"}


@app.post("/cart")
def update_cart(body: GenericJSON):
    return {"cart": body.data, "priced": True, "currency": "USD"}


@app.post("/events")
def ingest_events(batch: EventBatch):
    return {"batch_id": batch.batch_id, "received": len(batch.events)}


@app.post("/metrics")
def ingest_metrics(body: GenericJSON):
    series = body.data.get("series", [])
    return {"received_series": len(series), "sampled": False}


@app.post("/audit")
def ingest_audit(body: GenericJSON):
    events = body.data.get("events", [])
    return {"stored": len(events), "retention_days": 90}


@app.post("/comments")
def create_comment(body: GenericJSON):
    return {"comment": body.data, "moderation": "queued"}


@app.post("/support/tickets")
def create_ticket(ticket: SupportTicket):
    return {
        "ticket_id": "TCK-10001",
        "subject": ticket.subject,
        "priority": ticket.priority,
        "status": "open",
    }


@app.post("/notifications")
def send_notifications(body: GenericJSON):
    recipients = body.data.get("recipients", [])
    return {"queued": len(recipients), "provider": "local-pgo"}


@app.post("/echo")
def echo(body: GenericJSON):
    return {"received": body.data}


@app.post("/upload-json")
def upload_json(body: GenericJSON):
    size = len(str(body.data))
    return {"stored": True, "estimated_size": size}


@app.post("/middleware/probe")
def middleware_probe(body: GenericJSON):
    return {"seen": True, "payload": body.data}


# TODO: later - register a websocket workload once the PGO trainer has a
# stdlib-friendly websocket client or an optional dependency gate.


if __name__ == "__main__":
    app.serve("127.0.0.1", 8000)
