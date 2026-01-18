from fastrapi import FastrAPI
from fastrapi.websocket import websocket

app = FastrAPI()

@app.websocket("/ws")
async def websocket_endpoint(ws):
    await ws.accept()
    
    try:
        while True:
            data = await ws.receive_text()
            print(f"Client said: {data}")
            
            await ws.send_json({"reply": "Message received", "echo": data})
            
    except Exception as e:
        print(f"Connection closed: {e}")

if __name__ == "__main__":
    app.serve(host="0.0.0.0", port=8000)