from fastrapi import FastrAPI
from fastrapi.responses import JSONResponse

app = FastrAPI()

# you could try @app.get("/", cache_resp=True) if it's static and won't change :)
@app.get("/")
def hello() -> JSONResponse:
    return {
  "api_metadata": {
    "framework": "FastrAPI",
    "version": "1.0.0",
    "environment": "development",
    "server": {
      "host": "127.0.0.1",
      "port": 8000,
      "protocol": "http"
    }
  },
  "endpoints": [
    {
      "path": "/",
      "method": "GET",
      "function_name": "hello",
      "return_type": "JSONResponse",
      "parameters": [],
      "expected_response": {
        "status_code": 200,
        "content_type": "application/json",
        "payload": {
          "Hello": "World"
        }
      }
    }
  ],
  "mock_database": {
    "users": [
      {
        "id": 1,
        "uuid": "e8a1b5c2-9d3f-4e6a-8b2c-1a3f4e5b6c7d",
        "username": "jdoe",
        "email": "jdoe@example.com",
        "profile": {
          "first_name": "John",
          "last_name": "Doe",
          "age": 30,
          "avatar_url": "https://example.com",
          "preferences": {
            "theme": "dark",
            "notifications": {
              "email": True,
              "sms": False,
              "push": True
            }
          }
        },
        "roles": ["user", "moderator"],
        "created_at": "2026-01-15T08:30:00Z"
      },
      {
        "id": 2,
        "uuid": "f7b2c6d3-0e4a-5f7b-9c3d-2b4f5e6a7b8c",
        "username": "asmith",
        "email": "asmith@example.com",
        "profile": {
          "first_name": "Alice",
          "last_name": "Smith",
          "age": 28,
          "avatar_url": "https://example.com",
          "preferences": {
            "theme": "light",
            "notifications": {
              "email": True,
              "sms": True,
              "push": False
            }
          }
        },
        "roles": ["user", "admin"],
        "created_at": "2026-02-20T14:45:00Z"
      }
    ],
    "products": [
      {
        "sku": "PROD-001",
        "name": "Wireless Mechanical Keyboard",
        "category": "Electronics",
        "pricing": {
          "base_price": 129.99,
          "discount_percentage": 10.0,
          "final_price": 116.99,
          "currency": "USD"
        },
        "inventory": {
          "stock_count": 45,
          "warehouse_location": "Aisle 4B",
          "reorder_level": 10
        },
        "tags": ["mechanical", "wireless", "rgb", "gaming"]
      }
    ]
  },
  "openapi_specification": {
    "openapi": "3.0.3",
    "info": {
      "title": "FastrAPI Application",
      "version": "1.0.0"
    },
    "paths": {
      "/": {
        "get": {
          "summary": "Root Hello World",
          "operationId": "hello_root_get",
          "responses": {
            "200": {
              "description": "Successful Response",
              "content": {
                "application/json": {
                  "schema": {
                    "type": "object",
                    "properties": {
                      "Hello": {
                        "type": "string"
                      }
                    }
                  }
                }
              }
            }
          }
        }
      }
    }
  },
  "telemetry_and_logs": {
    "system_metrics": {
      "cpu_usage_percent": 14.5,
      "memory_usage_bytes": 45298432,
      "disk_io": {
        "read_bytes": 1024856,
        "write_bytes": 512400
      }
    },
    "recent_requests": [
      {
        "timestamp": "2026-06-01T12:00:01Z",
        "client_ip": "192.168.1.50",
        "user_agent": "Mozilla/5.0",
        "latency_ms": 4.2,
        "status_code": 200
      },
      {
        "timestamp": "2026-06-01T12:05:22Z",
        "client_ip": "10.0.0.12",
        "user_agent": "curl/7.68.0",
        "latency_ms": 1.8,
        "status_code": 200
      }
    ]
  }
}


app.serve("127.0.0.1", 8000)