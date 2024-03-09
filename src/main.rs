mod config;
mod radiator;


use std::borrow::Cow;
use std::collections::HashMap;
use std::convert::Infallible;
use std::ffi::OsString;
use std::fmt::{self, Write};
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
use tracing::{error, instrument, warn};

use crate::config::{CONFIG, Config};
use crate::radiator::{connect_to_radiator, Error, SOCKET_STATE, start_message_processor};


const GIT_REVISION: &str = "<unknown git revision>";


#[derive(Clone, Copy)]
enum Number {
    Integer(i64),
    Float(f64),
}
impl fmt::Display for Number {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Integer(v) => write!(f, "{}", v),
            Self::Float(v) => write!(f, "{}", v),
        }
    }
}


fn return_500() -> Result<Response<Full<Bytes>>, Infallible> {
    Ok(
        Response::builder()
            .status(500)
            .header("Content-Type", "text/plain; charset=utf-8")
            .body(Full::new(Bytes::from("internal server error")))
            .expect("cannot construct HTTP 500 response")
    )
}


fn escape_openmetrics_into(source: &str, destination: &mut String) {
    for c in source.chars() {
        if c == '\\' || c == '"' {
            destination.push('\\');
            destination.push(c);
        } else if c == '\n' {
            destination.push_str("\\n");
        } else {
            destination.push(c);
        }
    }
}


#[instrument(skip(request))]
async fn handle_request(request: Request<Incoming>, remote_addr: SocketAddr) -> Result<Response<Full<Bytes>>, Infallible> {
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
    let radiator_response = loop {
        match crate::radiator::communicate(b"STATS .").await {
            Ok(rr) => break rr,
            Err(Error::ReaderGone) => continue,
            Err(e) => {
                error!("failed to query Radiator: {}", e);
                return return_500();
            },
        }
    };

    // response format: b"STATS .\nkey1:value1\x01key2:value2\x01key3:value3"

    // skip echoed command
    let newline_index = match radiator_response.iter().position(|b| *b == b'\n') {
        Some(i) => i,
        None => {
            error!("Radiator response {:?} does not contain a newline (splitting echoed command and actual response)", radiator_response);
            return return_500();
        },
    };
    let unechoed_response = &radiator_response[newline_index+1..];

    // decode as UTF-8
    let response_string = match std::str::from_utf8(unechoed_response) {
        Ok(rs) => rs,
        Err(e) => {
            error!("Radiator response {:?} is not valid UTF-8: {}", radiator_response, e);
            return return_500();
        },
    };

    // key-value pairs are delimited by U+0001 characters
    let mut statistics = HashMap::new();
    let key_value_pairs = response_string.split('\u{0001}');
    for key_value_pair in key_value_pairs {
        // keys and values are delimited by a colon (let's assume the first one)
        let (key, value) = match key_value_pair.split_once(':') {
            Some(kv) => kv,
            None => {
                warn!("statistics key-value pair {:?} does not contain colon; skipping", key_value_pair);
                continue;
            },
        };

        // parse value
        let value = match value.parse() {
            Ok(v) => Number::Integer(v),
            Err(_) => {
                // integer failed; try float
                match value.parse() {
                    Ok(v) => Number::Float(v),
                    Err(e) => {
                        warn!("failed to parse value {:?} for statistic {:?} as an integer or floating-point value (skipping it): {}", value, key, e);
                        continue;
                    },
                }
            },
        };

        if let Some(old_value) = statistics.insert(key.to_owned(), value) {
            warn!("duplicate statistic {:?}; overwriting old value {} with {}", key, old_value, value);
        }
    }

    // prepare OpenMetrics output
    let mut output = String::new();
    let config = CONFIG
        .get().expect("CONFIG not set?!");
    for metric in &config.metrics {
        let mut first_sample = true;

        for sample in &metric.samples {
            // do we even have a value for this one?
            let Some(value) = statistics.get(&sample.statistic) else { continue };

            if first_sample {
                write!(output, "# TYPE {} {}\n", metric.metric, metric.kind.as_openmetrics()).unwrap();

                if let Some(unit) = metric.unit.as_ref() {
                    write!(output, "# UNIT {} {}\n", metric.metric, unit).unwrap();
                }

                if let Some(help) = metric.help.as_ref() {
                    write!(output, "# HELP {} ", metric.metric).unwrap();
                    escape_openmetrics_into(help, &mut output);
                    output.push('\n');
                }

                first_sample = false;
            }

            write!(output, "{}{}", metric.metric, metric.kind.openmetrics_metric_suffix()).unwrap();
            if sample.labels.len() > 0 {
                output.push('{');
                let mut first_label = true;
                for (label_key, label_value) in &sample.labels {
                    if first_label {
                        first_label = false;
                    } else {
                        output.push(',');
                    }
                    write!(output, "{}=\"", label_key).unwrap();
                    escape_openmetrics_into(label_value, &mut output);
                    output.push('"');
                }
                output.push('}');
            }

            write!(output, " {}\n", value).unwrap();
        }
    }
    output.push_str("# EOF\n");

    let response_res = Response::builder()
        .status(200)
        .header("Content-Type", "application/openmetrics-text; version=1.0.0; charset=utf-8")
        .body(Full::new(Bytes::from(output)));
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
        eprintln!("prometheus-radiator-exporter {} {}", env!("CARGO_PKG_VERSION"), GIT_REVISION);
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

    // launch the reader
    let mut socket_state = start_message_processor();

    // attempt initial connection to Radiator
    connect_to_radiator(&config.radiator, &mut socket_state).await
        .expect("failed to connect to Radiator management port");
    SOCKET_STATE
        .set(Mutex::new(socket_state)).expect("SOCKET_STATE already set?!");

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
                .serve_connection(io, service_fn(move |req| async move {
                    handle_request(req, remote_addr).await
                }))
                .await;
            if let Err(e) = connection_result {
                error!("server error while handling connection from {}: {}", remote_addr, e);
            }
        });
    }
}
