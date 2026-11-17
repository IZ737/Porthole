"""A minimal FastAPI backend for conformance.py: everything behind the auth."""

import os

import uvicorn
from fastapi import FastAPI, Request
from fastapi.responses import PlainTextResponse

from porthole_auth import PortholeAuth

app = FastAPI()
app.add_middleware(PortholeAuth)


@app.api_route("/{path:path}", methods=["GET", "POST", "HEAD"])
def anything(path: str, request: Request):
    return PlainTextResponse(f"hello {request.method} /{path}\n")


uvicorn.run(app, host="127.0.0.1", port=int(os.environ["APP_PORT"]), log_level="warning")
