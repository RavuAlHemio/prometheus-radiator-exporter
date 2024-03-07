mod config;
mod radiator;


use std::borrow::Cow;
use std::convert::Infallible;
use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::Path;
use std::process::ExitCode;

use http_body_util::Full;
use hyper::{Method, Request, Response};
use hyper::body::{Bytes, Incoming};
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use toml;
use tracing::error;

use crate::config::{CONFIG, Config};
use crate::radiator::{connect_to_radiator, RADIATOR_SOCKET};


fn return_500() -> Result<Response<Full<Bytes>>, Infallible> {
    Ok(
        Response::builder()
            .status(500)
            .header("Content-Type", "text/plain; charset=utf-8")
            .body(Full::new(Bytes::from("internal server error")))
            .expect("cannot construct HTTP 500 response")
    )
}


async fn handle_request(request: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    if request.method() != Method::GET {
        let response_res = Response::builder()
            .status(405)
            .header("Content-Type", "text/plain; charset=utf-8")
            .header("Allow", "GET")
            .body(Full::new(Bytes::from("HTTP method must be GET")));
        return match response_res {
            Ok(r) => Ok(r),
            Err(e) => {
                error!("failed to construct 405 response: {}", e);
                return_500()
            },
        };
    }

    // ask Radiator for top-level statistics
    let radiator_response = match crate::radiator::communicate(b"STATS .").await {
        Ok(rr) => rr,
        Err(e) => {
            error!("failed to query Radiator: {}", e);
            return return_500();
        },
    };

    // FIXME: reformat this into OpenMetrics
    let response_res = Response::builder()
        .status(200)
        .header("Content-Type", "application/octet-stream")
        .body(Full::new(Bytes::from(radiator_response)));
    match response_res {
        Ok(r) => Ok(r),
        Err(e) => {
            error!("failed to construct 200 response: {}", e);
            return_500()
        },
    }
}


#[tokio::main]
async fn main() -> ExitCode {
    // parse args
    let args: Vec<OsString> = std::env::args_os().collect();
    let mut prog_name = Cow::Borrowed("prometheus-radiator-exporter");
    if let Some(pn) = args.get(0) {
        prog_name = pn.to_string_lossy();
    }
    let output_usage =
        args.len() < 1
        || args.len() > 2
        || args.get(1)
            .map(|s| s.to_string_lossy().starts_with("-"))
            .unwrap_or(false);
    if output_usage {
        eprintln!("Usage: {} [CONFIG.TOML]", prog_name);
        return ExitCode::FAILURE;
    }
    let config_path = if let Some(config_path_os) = args.get(1) {
        Path::new(config_path_os)
    } else {
        Path::new("config.toml")
    };

    // load config
    let config: Config = {
        let config_string = std::fs::read_to_string(config_path)
            .expect("failed to load config file");
        toml::from_str(&config_string)
            .expect("failed to parse config file")
    };
    if let Err(e) = crate::config::check(&config) {
        panic!("error in configuration: {}", e);
    }
    CONFIG
        .set(config.clone()).expect("CONFIG already set?!");

    // enable tracing
    let (non_blocking_stdout, _guard) = tracing_appender::non_blocking(std::io::stdout());
    tracing_subscriber::fmt()
        .with_writer(non_blocking_stdout)
        .init();

    // attempt initial connection to Radiator
    let radiator_socket = connect_to_radiator(&config.radiator).await
        .expect("failed to connect to Radiator management port");
    RADIATOR_SOCKET
        .set(Mutex::new(radiator_socket)).expect("RADIATOR_SOCKET already set?!");

    // listen for HTTP
    let bind_addr = SocketAddr::from((config.www.bind_address, config.www.port));
    let listener = TcpListener::bind(bind_addr).await
        .expect("failed to create TCP listening socket");
    loop {
        let (stream, remote_addr) = listener.accept().await
            .expect("failed to accept incoming TCP connection");
        let io = TokioIo::new(stream);
        tokio::task::spawn(async move {
            let connection_result = Builder::new(TokioExecutor::new())
                .http1()
                .http2()
                .serve_connection(io, service_fn(handle_request))
                .await;
            if let Err(e) = connection_result {
                error!("server error while handling connection from {}: {}", remote_addr, e);
            }
        });
    }
}
