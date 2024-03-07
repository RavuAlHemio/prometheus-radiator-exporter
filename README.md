# prometheus-radiator-exporter

OpenMetrics (Prometheus) exporter for [Radiator](https://radiatorsoftware.com/products/radiator/) (RADIUS server).

Connects to Radiator via the Monitor interface, configured in a
[`<Monitor>` block](https://files.radiatorsoftware.com/radiator/ref/Monitor.html).

## Exporter configuration

The exporter is configured in a TOML file. By default, `prometheus-radiator-exporter` looks for a
file named `config.toml` in the current working directory; the path to an alternative configuration
file can be passed on the command line.

Sample configuration file:

```
[www]
bind_address = "0.0.0.0"
port = 10014

[radiator]
target = "127.0.0.1"
mgmt_port = 9000
username = "prom"
password = "Tr0ub4dor&3"
```
