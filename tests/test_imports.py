"""
FastrAPI Import & Basic Usage Smoke Test
Run with: pytest tests/test_imports.py -v
"""

import pytest
from pydantic import BaseModel


def test_top_level_imports():
    """Test the most common top-level imports (FastAPI style)"""
    from fastrapi import (
        FastrAPI,
        Depends,
        Query,
        Path,
        Body,
        HTTPException,
        BackgroundTasks,
    )
    
    assert FastrAPI is not None
    assert Depends is not None
    assert Query is not None
    assert Path is not None
    assert Body is not None
    assert HTTPException is not None
    assert BackgroundTasks is not None


def test_submodule_imports():
    """Test explicit submodule imports"""
    from fastrapi.responses import JSONResponse, HTMLResponse
    from fastrapi.exceptions import HTTPException as ExceptionsHTTPException
    from fastrapi.params import Depends as ParamsDepends, Query as ParamsQuery
    from fastrapi.background import BackgroundTasks as BgBackgroundTasks
    from fastrapi.middleware import CORSMiddleware

    assert JSONResponse is not None
    assert HTMLResponse is not None
    assert ExceptionsHTTPException is not None
    assert ParamsDepends is not None
    assert ParamsQuery is not None
    assert BgBackgroundTasks is not None
    assert CORSMiddleware is not None


def test_all_params_import():
    """Test all parameter classes"""
    from fastrapi.params import (
        Query, Path, Body, Header, Cookie, Form, File,
        Depends, Security, Unset, Undefined
    )
    assert all(cls is not None for cls in [Query, Path, Body, Header, Cookie, Form, File, 
                                           Depends, Security, Unset, Undefined])


def test_app_creation():
    """Test that FastrAPI app can be created"""
    from fastrapi import FastrAPI
    
    app = FastrAPI(title="Test App", version="0.1.0")
    assert app is not None
    assert app.title == "Test App"
    assert hasattr(app, "get")
    assert hasattr(app, "post")


class User(BaseModel):
    name: str
    age: int


def test_basic_decorators():
    """Test that decorators work"""
    from fastrapi import FastrAPI
    
    app = FastrAPI()
    
    @app.get("/")
    def root():
        return {"hello": "world"}
    
    @app.post("/users")
    def create_user(user: User):
        return user
    
    assert callable(root)
    assert callable(create_user)


def test_http_exception():
    """Test HTTPException usage"""
    from fastrapi import HTTPException
    
    with pytest.raises(HTTPException) as exc_info:
        raise HTTPException(status_code=404, detail="Not Found")
    
    assert exc_info.value.status_code == 404
    assert exc_info.value.detail == "Not Found"


def test_background_tasks_import_and_usage():
    """Test BackgroundTasks can be imported and instantiated"""
    from fastrapi.background import BackgroundTasks
    
    tasks = BackgroundTasks()
    assert tasks is not None
    assert hasattr(tasks, "add_task")



def test_openapi_endpoint_exists():
    """Basic check that OpenAPI route is registered"""
    from fastrapi import FastrAPI
    
    app = FastrAPI()
    
    assert hasattr(app, "get")


if __name__ == "__main__":
    pytest.main([__file__, "-v"])