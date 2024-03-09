mod config;
mod openmetrics;
mod radiator;


use std::borrow::Cow;
use std::collections::HashMap;
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
use tracing::{error, instrument, warn};

use crate::config::{CONFIG, Config};
use crate::openmetrics::{MetricDatabase, Number};
use crate::radiator::{connect_to_radiator, SOCKET_STATE, start_message_processor};


const GIT_REVISION: &str = "<unknown git revision>";


#[derive(Clone, Debug)]
struct PerObjectStats {
    pub identifier: String,
    pub stats: HashMap<String, Number>,
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


fn decode_stats(response: &[u8]) -> Option<HashMap<String, Number>> {
    // response format: b"STATS .\nkey1:value1\x01key2:value2\x01key3:value3"

    // skip echoed command
    let newline_index = match response.iter().position(|b| *b == b'\n') {
        Some(i) => i,
        None => {
            error!("Radiator response {:?} does not contain a newline (splitting echoed command and actual response)", response);
            return None;
        },
    };
    let unechoed_response = &response[newline_index+1..];

    // decode as UTF-8
    let response_string = match std::str::from_utf8(unechoed_response) {
        Ok(rs) => rs,
        Err(e) => {
            error!("Radiator response {:?} is not valid UTF-8: {}", response, e);
            return None;
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

    Some(statistics)
}


fn extract_identifier(response: &[u8]) -> Option<String> {
    // response format: b"DESCRIBE ObjectType.2\nkey1:type1:value1\x01key2:type2:value2\x01key3:type3:value3"

    // skip echoed command
    let newline_index = match response.iter().position(|b| *b == b'\n') {
        Some(i) => i,
        None => {
            error!("Radiator response {:?} does not contain a newline (splitting echoed command and actual response)", response);
            return None;
        },
    };
    let unechoed_response = &response[newline_index+1..];

    // decode as UTF-8
    let response_string = match std::str::from_utf8(unechoed_response) {
        Ok(rs) => rs,
        Err(e) => {
            error!("Radiator response {:?} is not valid UTF-8: {}", response, e);
            return None;
        },
    };

    // key-type-value tuples are delimited by U+0001 characters
    let key_type_value_tuples = response_string.split('\u{0001}');
    for key_type_value_tuple in key_type_value_tuples {
        // keys, types and values are delimited by colons (the first two)
        let (key, type_value_pair) = match key_type_value_tuple.split_once(':') {
            Some(ktvp) => ktvp,
            None => {
                warn!("statistics key-type-value tuple {:?} does not contain colon; skipping", key_type_value_tuple);
                continue;
            },
        };
        let (value_type, value) = match type_value_pair.split_once(':') {
            Some(tv) => tv,
            None => {
                warn!("statistics key-type-value tuple {:?} does not contain second colon; skipping", key_type_value_tuple);
                continue;
            },
        };

        if key == "Identifier" && value_type == "string" {
            return Some(value.to_owned());
        }
    }

    None
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

    let mut metric_database = MetricDatabase::new();

    // ask Radiator for top-level statistics
    let radiator_response = match crate::radiator::communicate(b"STATS .").await {
        Ok(rr) => rr,
        Err(e) => {
            error!("failed to query Radiator global stats: {}", e);
            return return_500();
        },
    };
    let statistics = match decode_stats(&radiator_response) {
        Some(s) => s,
        None => {
            // error already output
            return return_500();
        },
    };

    let config = CONFIG
        .get().expect("CONFIG not set?!");

    // run through per-object statistics
    let mut object_type_to_statistics: HashMap<String, HashMap<usize, PerObjectStats>> = HashMap::new();
    for per_object_statistic in &config.per_object_metrics {
        // query the identifiers
        let mut index_to_identifier: HashMap<usize, String> = HashMap::new();
        for i in 0.. {
            let command = format!("DESCRIBE {}.{}", per_object_statistic.kind, i);
            let radiator_response = match crate::radiator::communicate(command.as_bytes()).await {
                Ok(rr) => rr,
                Err(e) => {
                    error!("failed to query Radiator info for {}.{}: {}", per_object_statistic.kind, i, e);
                    return return_500();
                },
            };
            if radiator_response == b"NOSUCHOBJECT" {
                // that is all
                break;
            }
            let identifier = match extract_identifier(&radiator_response) {
                Some(id) => id,
                None => {
                    warn!("Radiator object {}.{} does not have an identifier; skipping", per_object_statistic.kind, i);
                    continue;
                },
            };
            index_to_identifier.insert(i, identifier);
        }

        // pull statistics for each object
        let mut index_to_statistics = HashMap::new();
        for (&index, identifier) in &index_to_identifier {
            let command = format!("STATS {}.{}", per_object_statistic.kind, index);
            let radiator_response = match crate::radiator::communicate(command.as_bytes()).await {
                Ok(rr) => rr,
                Err(e) => {
                    error!("failed to query Radiator stats for {}.{}: {}", per_object_statistic.kind, index, e);
                    return return_500();
                },
            };
            let stats = match decode_stats(&radiator_response) {
                Some(s) => s,
                None => {
                    // error already output
                    return return_500();
                },
            };
            let per_object_stats = PerObjectStats {
                identifier: identifier.clone(),
                stats,
            };
            index_to_statistics.insert(index, per_object_stats);
        }

        object_type_to_statistics.insert(per_object_statistic.kind.clone(), index_to_statistics);
    }

    // populate metrics database
    for metric_config in &config.metrics {
        let metric = metric_database.get_or_insert(&metric_config.metric, metric_config.kind);
        metric.set_unit(metric_config.unit.clone());
        metric.set_help(metric_config.help.clone());
        for sample in &metric_config.samples {
            for label_name in sample.labels.keys() {
                if !metric.has_label(label_name) {
                    metric.add_label(label_name.to_owned());
                }
            }
        }

        for sample in &metric_config.samples {
            let value = match statistics.get(&sample.statistic) {
                Some(v) => v,
                None => continue,
            };
            metric.add_sample(&sample.labels, value.clone());
        }
    }
    for per_object_metrics in &config.per_object_metrics {
        let Some(index_to_statistics) = object_type_to_statistics.get(&per_object_metrics.kind)
            else { continue };
        for metric_config in &per_object_metrics.metrics {
            let metric = metric_database.get_or_insert(&metric_config.metric, metric_config.kind);
            metric.set_unit(metric_config.unit.clone());
            metric.set_help(metric_config.help.clone());
            for sample in &metric_config.samples {
                for label_name in sample.labels.keys() {
                    if !metric.has_label(label_name) {
                        metric.add_label(label_name.to_owned());
                    }
                }
            }
            metric.add_label(per_object_metrics.identifier_label.clone());

            for per_object_statistics in index_to_statistics.values() {
                for sample in &metric_config.samples {
                    let mut all_labels = sample.labels.clone();
                    all_labels.insert(per_object_metrics.identifier_label.clone(), per_object_statistics.identifier.clone());
                    let value = match per_object_statistics.stats.get(&sample.statistic) {
                        Some(v) => v,
                        None => continue,
                    };
                    metric.add_sample(&all_labels, value.clone());
                }
            }
        }
    }

    // collect the output
    let mut output = String::new();
    if let Err(e) = metric_database.write(&mut output) {
        error!("error collecting metrics output: {}", e);
        return return_500();
    }
    output.push_str("# EOF\n");

    let response_res = Response::builder()
        .status(200)
        .header("Content-Type", crate::openmetrics::MIME_TYPE)
        .header("Content-Length", &output.len().to_string())
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
