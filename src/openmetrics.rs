use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};


pub const MIME_TYPE: &str = "application/openmetrics-text; version=1.0.0; charset=utf-8";


#[derive(Clone, Debug, Default)]
pub struct MetricDatabase {
    name_to_metric: BTreeMap<String, Metric>,
}
impl MetricDatabase {
    pub fn new() -> Self {
        Self {
            name_to_metric: BTreeMap::new(),
        }
    }

    pub fn get_or_insert(&mut self, name: &str, kind: MetricKind) -> &mut Metric {
        self.name_to_metric.entry(name.to_owned())
            .or_insert_with(|| Metric::new(name.to_owned(), kind))
    }

    pub fn metrics(&self) -> impl Iterator<Item = (&String, &Metric)> {
        self.name_to_metric.iter()
    }

    pub fn write<W: fmt::Write>(&self, mut writer: W) -> Result<(), fmt::Error> {
        for metric in self.name_to_metric.values() {
            metric.write(&mut writer)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct Metric {
    name: String,
    kind: MetricKind,
    help: Option<String>,
    unit: Option<String>,
    label_names: BTreeSet<String>,
    samples: BTreeMap<Vec<String>, Number>,
}
impl Metric {
    pub fn new(name: String, kind: MetricKind) -> Self {
        // metricname = metricname-initial-char 0*metricname-char
        // metricname-char = metricname-initial-char / DIGIT
        // metricname-initial-char = ALPHA / "_" / ":"
        assert!(name.len() > 0);
        let first_name_char = name.chars().nth(0).unwrap();
        assert!(first_name_char.is_ascii_alphabetic() || first_name_char == '_' || first_name_char == ':');
        assert!(name.chars().skip(1).all(|c| c.is_ascii_alphabetic() || c.is_ascii_digit() || c == '_' || c == ':'));

        Self {
            name,
            kind,
            help: None,
            unit: None,
            label_names: BTreeSet::new(),
            samples: BTreeMap::new(),
        }
    }

    pub fn set_help(&mut self, help: Option<String>) {
        if let Some(help_str) = help.as_ref() {
            assert!(help_str.len() > 0);
            // otherwise, help string may be anything
        }

        self.help = help;
    }

    pub fn set_unit(&mut self, unit: Option<String>) {
        // all are metricname-char
        if let Some(unit_str) = unit.as_ref() {
            assert!(unit_str.len() > 0);
            assert!(unit_str.chars().all(|c| c.is_ascii_alphabetic() || c.is_ascii_digit() || c == '_' || c == ':'));
        }

        self.unit = unit;
    }

    pub fn has_label(&self, label: &str) -> bool {
        self.label_names.contains(label)
    }

    pub fn add_label(&mut self, label: String) {
        assert!(self.samples.is_empty());

        // label-name = label-name-initial-char *label-name-char
        // label-name-char = label-name-initial-char / DIGIT
        // label-name-initial-char = ALPHA / "_"
        let first_label_char = label.chars().nth(0).unwrap();
        assert!(first_label_char.is_ascii_alphabetic() || first_label_char == '_');
        assert!(label.chars().skip(1).all(|c| c.is_ascii_alphabetic() || c.is_ascii_digit() || c == '_'));

        self.label_names.insert(label);
    }

    pub fn add_sample(&mut self, labels: &BTreeMap<String, String>, value: Number) {
        let mut label_values = Vec::with_capacity(self.label_names.len());
        for label_name in &self.label_names {
            let label_value = match labels.get(label_name) {
                Some(lv) => lv,
                None => panic!("missing value for label {:?}", label_name),
            };
            label_values.push(label_value.clone());
        }
        for key in labels.keys() {
            if !self.label_names.contains(key) {
                panic!("unknown label {:?}", key);
            }
        }
        self.samples.insert(label_values, value);
    }

    pub fn write<W: fmt::Write>(&self, mut writer: W) -> Result<(), fmt::Error> {
        write!(writer, "# TYPE {} {}\n", self.name, self.kind.as_openmetrics())?;

        if let Some(unit) = self.unit.as_ref() {
            write!(writer, "# UNIT {} {}\n", self.name, unit)?;
        }

        if let Some(help) = self.help.as_ref() {
            write!(writer, "# HELP {} ", self.name)?;
            escape_openmetrics_into(help, &mut writer)?;
            write!(writer, "\n")?;
        }

        for (label_values, sample_value) in &self.samples {
            assert_eq!(self.label_names.len(), label_values.len());

            write!(writer, "{}{}", self.name, self.kind.openmetrics_metric_suffix())?;
            if self.label_names.len() > 0 {
                write!(writer, "{}", '{')?;
                let mut first_label = true;
                for (label_key, label_value) in self.label_names.iter().zip(label_values.iter()) {
                    if first_label {
                        first_label = false;
                    } else {
                        write!(writer, ",")?;
                    }
                    write!(writer, "{}=\"", label_key)?;
                    escape_openmetrics_into(label_value, &mut writer)?;
                    write!(writer, "\"")?;
                }
                write!(writer, "{}", '}')?;
            }
            write!(writer, " {}\n", sample_value)?;
        }

        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricKind {
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

#[derive(Clone, Copy, Debug)]
pub enum Number {
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


pub(crate) fn escape_openmetrics_into<W: fmt::Write>(source: &str, mut writer: W) -> Result<(), fmt::Error> {
    for c in source.chars() {
        if c == '\\' || c == '"' {
            write!(writer, "\\{}", c)?;
        } else if c == '\n' {
            write!(writer, "\\n")?;
        } else {
            write!(writer, "{}", c)?;
        }
    }
    Ok(())
}
