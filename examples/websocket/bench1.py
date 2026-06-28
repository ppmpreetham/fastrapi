from fastrapi import FastrAPI

app = FastrAPI()

@app.websocket("/ws")
async def websocket_endpoint(ws):
    await ws.accept()

    try:
        while True:
            await ws.receive_text()
            await ws.send_text("ok")
    except Exception:
        pass

if __name__ == "__main__":
    app.serve(host="0.0.0.0", port=8000)