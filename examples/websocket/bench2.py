from fastrapi import FastrAPI
import json

app = FastrAPI()

@app.websocket("/ws")
async def websocket_endpoint(ws):
    await ws.accept()

    try:
        while True:
            raw = await ws.receive_text()

            obj = json.loads(raw)

            response = json.dumps({
                "received": obj
            })

            await ws.send_text(response)

    except Exception:
        pass

if __name__ == "__main__":
    app.serve(host="0.0.0.0", port=8000)