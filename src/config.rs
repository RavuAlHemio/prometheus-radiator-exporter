use std::borrow::Cow;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};


pub(crate) static CONFIG: OnceLock<Config> = OnceLock::new();


#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub(crate) struct Config {
    pub www: WwwConfig,
    pub radiator: RadiatorConfig,
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
    const fn default_port() -> u16 { 10013 }
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

    Ok(())
}
