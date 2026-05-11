def test_top_level_imports():
    from fastrapi import (
        BackgroundTasks,
        Body,
        Cookie,
        Depends,
        FastrAPI,
        File,
        Form,
        Header,
        HTTPException,
        Path,
        Query,
        Security,
        SecurityScopes,
        UploadFile,
    )

    assert FastrAPI.__name__ == "FastrAPI"
    assert Depends.__name__ == "Depends"
    assert Query.__name__ == "Query"
    assert Path.__name__ == "Path"
    assert Body.__name__ == "Body"
    assert Header.__name__ == "Header"
    assert Cookie.__name__ == "Cookie"
    assert Form.__name__ == "Form"
    assert File.__name__ == "File"
    assert Security.__name__ == "Security"
    assert SecurityScopes.__name__ == "SecurityScopes"
    assert UploadFile.__name__ == "UploadFile"
    assert HTTPException.__name__ == "HTTPException"
    assert BackgroundTasks.__name__ == "BackgroundTasks"


def test_submodule_imports():
    from fastrapi.background import BackgroundTasks
    from fastrapi.datastructures import UploadFile
    from fastrapi.exceptions import (
        FastrAPIError,
        HTTPException,
        RequestValidationError,
        ResponseValidationError,
        ValidationException,
        WebSocketException,
    )
    from fastrapi.middleware import (
        CORSMiddleware,
        GZipMiddleware,
        SessionMiddleware,
        TrustedHostMiddleware,
    )
    from fastrapi.params import Body, Depends, Header, Path, Query, Security
    from fastrapi.request import HTTPConnection, Request
    from fastrapi.responses import (
        HTMLResponse,
        JSONResponse,
        PlainTextResponse,
        RedirectResponse,
    )
    from fastrapi.security import SecurityScopes
    from fastrapi.websocket import WebSocket, websocket

    imported = [
        BackgroundTasks,
        UploadFile,
        FastrAPIError,
        HTTPException,
        ValidationException,
        RequestValidationError,
        ResponseValidationError,
        WebSocketException,
        CORSMiddleware,
        GZipMiddleware,
        SessionMiddleware,
        TrustedHostMiddleware,
        Body,
        Depends,
        Header,
        Path,
        Query,
        Security,
        Request,
        HTTPConnection,
        HTMLResponse,
        JSONResponse,
        PlainTextResponse,
        RedirectResponse,
        SecurityScopes,
        WebSocket,
        websocket,
    ]
    assert all(obj is not None for obj in imported)


def test_status_module_exports_http_and_websocket_codes():
    from fastrapi import status

    assert status.HTTP_200_OK == 200
    assert status.HTTP_201_CREATED == 201
    assert status.HTTP_404_NOT_FOUND == 404
    assert status.HTTP_422_UNPROCESSABLE_CONTENT == 422
    assert status.HTTP_500_INTERNAL_SERVER_ERROR == 500
    assert status.WS_1000_NORMAL_CLOSURE == 1000
    assert status.WS_1008_POLICY_VIOLATION == 1008
