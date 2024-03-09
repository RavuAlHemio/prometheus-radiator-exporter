use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::openmetrics::MetricKind;


pub(crate) static CONFIG: OnceLock<Config> = OnceLock::new();


#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub(crate) struct Config {
    pub www: WwwConfig,
    pub radiator: RadiatorConfig,
    pub metrics: Vec<MetricConfig>,
    #[serde(default)] pub per_object_metrics: Vec<PerObjectMetricConfig>,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub(crate) struct WwwConfig {
    #[serde(default = "WwwConfig::default_bind_address")]
    pub bind_address: IpAddr,

    #[serde(default = "WwwConfig::default_port")]
    pub port: u16,
}
impl WwwConfig {
    const fn default_bind_address() -> IpAddr { IpAddr::V6(Ipv6Addr::UNSPECIFIED) }
    const fn default_port() -> u16 { 10014 }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub(crate) struct RadiatorConfig {
    #[serde(default = "RadiatorConfig::default_target")]
    pub target: IpAddr,

    pub mgmt_port: u16,

    pub username: String,

    pub password: String,
}
impl RadiatorConfig {
    const fn default_target() -> IpAddr { IpAddr::V4(Ipv4Addr::LOCALHOST) }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub(crate) struct MetricConfig {
    pub metric: String,
    pub kind: MetricKind,
    #[serde(default)] pub help: Option<String>,
    #[serde(default)] pub unit: Option<String>,
    pub samples: Vec<SampleConfig>,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub(crate) struct PerObjectMetricConfig {
    pub kind: String,
    pub identifier_label: String,
    pub metrics: Vec<MetricConfig>,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub(crate) struct SampleConfig {
    #[serde(default)] pub labels: BTreeMap<String, String>,
    pub statistic: String,
}


pub(crate) fn check(config: &Config) -> Result<(), Cow<'static, str>> {
    if config.radiator.username.contains(' ') || config.radiator.username.contains('\0') {
        return Err(Cow::Borrowed("radiator.username must not contain spaces"));
    }
    if config.radiator.username.contains('\0') {
        return Err(Cow::Borrowed("radiator.username must not contain NUL characters"));
    }
    if config.radiator.password.contains(' ') {
        return Err(Cow::Borrowed("radiator.password must not contain spaces"));
    }
    if config.radiator.password.contains('\0') {
        return Err(Cow::Borrowed("radiator.password must not contain NUL characters"));
    }

    let mut known_metrics = HashSet::new();

    for (i, metric) in config.metrics.iter().enumerate() {
        let base = format!("metrics[{}]", i);
        check_metric(metric, &base, &mut known_metrics)?;
    }

    let mut known_objects = HashSet::new();

    for (i, per_object_metric) in config.per_object_metrics.iter().enumerate() {
        if !known_objects.insert(&per_object_metric.kind) {
            return Err(Cow::Owned(format!("per_object_metrics[{}].kind is not unique", i)));
        }

        for (j, metric) in per_object_metric.metrics.iter().enumerate() {
            let base = format!("per_object_metrics[{}].metrics[{}]", i, j);
            check_metric(metric, &base, &mut known_metrics)?;
        }
    }

    Ok(())
}

fn check_metric<'a>(metric: &'a MetricConfig, base: &str, known_metrics: &mut HashSet<&'a String>) -> Result<(), Cow<'static, str>> {
    if !known_metrics.insert(&metric.metric) {
        return Err(Cow::Owned(format!("{}.metric is not unique", base)));
    }

    if metric.metric.len() == 0 {
        return Err(Cow::Owned(format!("{}.metric must not be empty", base)));
    }

    let metric_start = metric.metric.chars().nth(0).unwrap();
    if !(metric_start.is_ascii_alphabetic() || metric_start == '_' && metric_start == ':') {
        return Err(Cow::Owned(format!("{}.metric must start with an ASCII letter, an underscore or a colon", base)));
    }

    let metric_rest_is_valid = metric.metric.chars()
        .skip(1)
        .all(|c| c.is_ascii_alphabetic() || c.is_ascii_digit() || c == '_' || c == ':');
    if !metric_rest_is_valid {
        return Err(Cow::Owned(format!("{}.metric must consist only of ASCII letters, ASCII digits, underscores and colons", base)));
    }

    // help string may contain anything :-)

    if let Some(unit) = metric.unit.as_ref() {
        let unit_is_valid = unit.chars()
            .all(|c| c.is_ascii_alphabetic() || c.is_ascii_digit() || c == '_' || c == ':');
        if !unit_is_valid {
            return Err(Cow::Owned(format!("{}.unit must be null or consist only of ASCII letters, ASCII digits, underscores and colons", base)));
        }
    }

    for (j, sample) in metric.samples.iter().enumerate() {
        if sample.statistic.find(':').is_some() {
            return Err(Cow::Owned(format!("{}.samples[{}].statistic must not contain a colon", base, j)));
        }

        for (key, _value) in &sample.labels {
            if key.len() == 0 {
                return Err(Cow::Owned(format!("{}.samples[{}].labels[{:?}] key must not be empty", base, j, key)));
            }

            let key_start = key.chars().nth(0).unwrap();
            if !(key_start.is_ascii_alphabetic() || key_start == '_') {
                return Err(Cow::Owned(format!("{}.samples[{}].labels[{:?}] key must start with an ASCII letter or an underscore", base, j, key)));
            }

            let key_rest_is_valid = key.chars()
                .skip(1)
                .all(|c| c.is_ascii_alphabetic() || c.is_ascii_digit() || c == '_');
            if !key_rest_is_valid {
                return Err(Cow::Owned(format!("{}.samples[{}].labels[{:?}] key must consist only of ASCII letters, ASCII digits and underscores", base, j, key)));
            }

            // values may contain anything :-)
        }
    }

    Ok(())
}
