use axum::extract::connect_info::Connected;
use axum::serve::IncomingStream;
use std::net::SocketAddr;
use tokio::net::TcpListener;

#[derive(Clone, Copy, Debug)]
pub struct PeerAddrs {
    pub remote: SocketAddr,
    pub local: Option<SocketAddr>,
}

impl Connected<IncomingStream<'_, TcpListener>> for PeerAddrs {
    fn connect_info(stream: IncomingStream<'_, TcpListener>) -> Self {
        _ = stream.io().set_nodelay(true);
        Self {
            remote: *stream.remote_addr(),
            local: stream.io().local_addr().ok(),
        }
    }
}
