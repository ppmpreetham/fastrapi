from fastrapi import FastrAPI, File, Form, UploadFile

app = FastrAPI()


@app.post("/upload")
def upload(file: UploadFile = File(), description: str = Form(default="")):
    return {
        "filename": file.filename,
        "content_type": file.content_type,
        "size": file.size,
        "description": description,
    }


app.serve("127.0.0.1", 8000)
