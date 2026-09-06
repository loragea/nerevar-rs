mod routes;
pub mod state;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use log::info;

pub use routes::router;

pub async fn try_bind(port: i32) -> Result<tokio::net::TcpListener, String> {
    let addr = format!("0.0.0.0:{port}");
    tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|error| format!("Failed to bind to {addr}: {error}"))
}

pub async fn serve(
    listener: tokio::net::TcpListener,
    ctx: Arc<state::ServerContext>,
) -> Result<(), String> {
    let app = router(ctx);
    let addr = listener.local_addr().map_err(|error| error.to_string())?;
    info!("NEREVAR SERVER: listening on {addr}");

    axum::serve(listener, app)
        .await
        .map_err(|error| error.to_string())
}

pub async fn start_web_server_on_port(
    port: i32,
    ctx: Arc<state::ServerContext>,
) -> Result<(), String> {
    let listener = try_bind(port).await?;
    serve(listener, ctx).await
}

/// What [`Transport::serve`] hands back: the running server, boxed so the
/// trait stays object-safe.
pub type ServeFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

/// How a bound listener becomes a running HTTP service.
///
/// Every route, extractor, and piece of state is shared no matter which
/// implementation is in play; only the bytes on the wire differ. The desktop
/// app and a daemon with no certificate configured use [`PlainHttp`];
/// `nerevar-host` implements this trait over rustls when the operator supplies
/// one. Keeping it a trait is the point: the server-side TLS stack lives
/// entirely in the daemon's crate, so core — and therefore the desktop app —
/// never links one.
pub trait Transport: Send + Sync + 'static {
    fn serve(
        &self,
        listener: tokio::net::TcpListener,
        ctx: Arc<state::ServerContext>,
    ) -> ServeFuture;
}

/// Plain HTTP: [`serve`] behind the [`Transport`] trait, and the only
/// transport core itself provides.
pub struct PlainHttp;

impl Transport for PlainHttp {
    fn serve(
        &self,
        listener: tokio::net::TcpListener,
        ctx: Arc<state::ServerContext>,
    ) -> ServeFuture {
        Box::pin(serve(listener, ctx))
    }
}
