use axum::{Router, routing::get};

async fn hello_world() -> &'static str {
    "Hello, World!"
}

#[tokio::main(flavor = "multi_thread", worker_threads = 1)]
async fn main() {
    let app = Router::new().route("/", get(hello_world));
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8000").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
