from fastrapi.encoders import jsonable_encoder
from fastrapi.responses import JSONResponse, Response
from fastrapi.utils import is_body_allowed_for_status_code
from starlette.status import WS_1008_POLICY_VIOLATION


async def http_exception_handler(request, exc):
    headers = getattr(exc, "headers", None)
    if not is_body_allowed_for_status_code(exc.status_code):
        return Response(status_code=exc.status_code, headers=headers)
    return JSONResponse({"detail": exc.detail}, status_code=exc.status_code, headers=headers)


async def request_validation_exception_handler(request, exc):
    return JSONResponse(status_code=422, content={"detail": jsonable_encoder(exc.errors())})


async def websocket_request_validation_exception_handler(websocket, exc):
    await websocket.close(code=WS_1008_POLICY_VIOLATION, reason=jsonable_encoder(exc.errors()))
