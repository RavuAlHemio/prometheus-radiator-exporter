use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};


pub(crate) static CONFIG: OnceLock<Config> = OnceLock::new();


#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub(crate) struct Config {
    pub www: WwwConfig,
    pub radiator: RadiatorConfig,
    pub metrics: Vec<MetricConfig>,
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
pub(crate) struct SampleConfig {
    #[serde(default)] pub labels: BTreeMap<String, String>,
    pub statistic: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MetricKind {
    Counter,
    Gauge,
}
impl MetricKind {
    pub const fn as_openmetrics(&self) -> &'static str {
        match self {
            Self::Counter => "counter",
            Self::Gauge => "gauge",
        }
    }

    pub const fn openmetrics_metric_suffix(&self) -> &'static str {
        match self {
            Self::Counter => "_total",
            Self::Gauge => "",
        }
    }
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

    let mut known_metrics = HashSet::with_capacity(config.metrics.len());
    for (i, metric) in config.metrics.iter().enumerate() {
        if !known_metrics.insert(&metric.metric) {
            return Err(Cow::Owned(format!("metrics[{}].metric is not unique", i)));
        }

        if metric.metric.len() == 0 {
            return Err(Cow::Owned(format!("metrics[{}].metric must not be empty", i)));
        }

        let metric_start = metric.metric.chars().nth(0).unwrap();
        if !(metric_start.is_ascii_alphabetic() || metric_start == '_' && metric_start == ':') {
            return Err(Cow::Owned(format!("metrics[{}].metric must start with an ASCII letter, an underscore or a colon", i)));
        }

        let metric_rest_is_valid = metric.metric.chars()
            .skip(1)
            .all(|c| c.is_ascii_alphabetic() || c.is_ascii_digit() || c == '_' || c == ':');
        if !metric_rest_is_valid {
            return Err(Cow::Owned(format!("metrics[{}].metric must consist only of ASCII letters, ASCII digits, underscores and colons", i)));
        }

        // help string may contain anything :-)

        if let Some(unit) = metric.unit.as_ref() {
            let unit_is_valid = unit.chars()
                .all(|c| c.is_ascii_alphabetic() || c.is_ascii_digit() || c == '_' || c == ':');
            if !unit_is_valid {
                return Err(Cow::Owned(format!("metrics[{}].unit must be null or consist only of ASCII letters, ASCII digits, underscores and colons", i)));
            }
        }

        for (j, sample) in metric.samples.iter().enumerate() {
            if sample.statistic.find(':').is_some() {
                return Err(Cow::Owned(format!("metrics[{}].samples[{}].statistic must not contain a colon", i, j)));
            }

            for (key, _value) in &sample.labels {
                if key.len() == 0 {
                    return Err(Cow::Owned(format!("metrics[{}].samples[{}].labels[{:?}] key must not be empty", i, j, key)));
                }
    
                let key_start = key.chars().nth(0).unwrap();
                if !(key_start.is_ascii_alphabetic() || key_start == '_') {
                    return Err(Cow::Owned(format!("metrics[{}].samples[{}].labels[{:?}] key must start with an ASCII letter or an underscore", i, j, key)));
                }
    
                let key_rest_is_valid = key.chars()
                    .skip(1)
                    .all(|c| c.is_ascii_alphabetic() || c.is_ascii_digit() || c == '_');
                if !key_rest_is_valid {
                    return Err(Cow::Owned(format!("metrics[{}].samples[{}].labels[{:?}] key must consist only of ASCII letters, ASCII digits and underscores", i, j, key)));
                }
    
                // values may contain anything :-)
            }
        }
    }

    Ok(())
}
