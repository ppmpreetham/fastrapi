from fastrapi import FastrAPI, Form

app = FastrAPI(
    max_body_size=1024,
    max_field_size=4,
    max_file_size=16,
    reject_unknown_multipart_fields=True,
)


@app.post("/upload")
def upload(description: str = Form(...)):
    return {"description": description}


if __name__ == "__main__":
    app.serve("127.0.0.1", 8000)
