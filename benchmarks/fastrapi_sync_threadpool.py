from fastrapi import FastrAPI

# Offload sync handlers to Tokio's thread pool.
#
# Recommended for:
# - time.sleep()
# - database drivers
# - file I/O
# - NumPy/Pandas workloads
# - other operations that release the GIL or block
#
# Usually NOT beneficial for:
# - pure Python CPU-bound loops
# - computational work that holds the GIL
app = FastrAPI(sync_to_threadpool=True)


@app.get("/")
def hello():
    return {"Hello": "World"}


if __name__ == "__main__":
    app.serve("127.0.0.1", 8000)
