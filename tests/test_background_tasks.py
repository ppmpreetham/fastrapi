# tests/test_background_tasks.py
from fastrapi import BackgroundTasks


def test_background_task(client, app, caplog):
    background_called = False

    def background_work():
        nonlocal background_called
        background_called = True

    @app.post("/task")
    def create_task(background_tasks: BackgroundTasks):
        background_tasks.add_task(background_work)
        return {"status": "accepted"}

    response = client.post("/task")
    assert response.status_code == 202  # or 200 depending on your impl
    assert background_called is True  # runs after response