"""A minimal Starlette backend for conformance.py, wrapped as plain ASGI."""

import os

import uvicorn
from starlette.applications import Starlette
from starlette.responses import PlainTextResponse
from starlette.routing import Route

from porthole_auth import PortholeAuth


async def anything(request):
    return PlainTextResponse(f"hello {request.method} {request.url.path}\n")


app = PortholeAuth(
    Starlette(routes=[Route("/{path:path}", anything, methods=["GET", "POST", "HEAD"])])
)
uvicorn.run(app, host="127.0.0.1", port=int(os.environ["APP_PORT"]), log_level="warning")
