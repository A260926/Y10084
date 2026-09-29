//! Writer-agnostic descriptor and label traversal core shared by the
//! [`Registry`](crate::registry::Registry) traversal and all exposition
//! format encoders.
//!
//! The core owns the semantics that must stay identical across formats:
//!
//! - final metric name composition from prefix, name and unit
//!   ([`Descriptor`]),
//! - const label merging in registration order ([`ConstLabels`]),
//! - stable ordering of const, additional and family label groups
//!   ([`LabelOrder`]).
//!
//! Format specific concerns (HELP/TYPE/UNIT/EOF lines, protobuf message
//! construction) stay in the per-format adapters.

use std::borrow::Cow;
use std::fmt::Write;

use crate::registry::{Prefix, Unit};

/// Writer-agnostic scope of a (sub-)registry: its metric name prefix and its
/// const labels.
///
/// A [`Scope`] is a cheaply copyable view; it borrows the prefix and labels
/// from the [`Registry`](crate::registry::Registry) and thus never allocates,
/// no matter how deeply sub-registries are nested.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Scope<'a> {
    prefix: Option<&'a Prefix>,
    const_labels: ConstLabels<'a>,
}

impl<'a> Scope<'a> {
    pub(crate) fn new(
        prefix: Option<&'a Prefix>,
        const_labels: &'a [(Cow<'static, str>, Cow<'static, str>)],
    ) -> Self {
        Self {
            prefix,
            const_labels: ConstLabels(const_labels),
        }
    }

    /// Combine this scope with a metric name and unit into the final
    /// [`Descriptor`] shared by all encoders.
    pub(crate) fn descriptor(&self, name: &'a str, unit: Option<&'a Unit>) -> Descriptor<'a> {
        Descriptor {
            prefix: self.prefix,
            name,
            unit,
            const_labels: self.const_labels,
        }
    }
}

/// Writer-agnostic view of a fully resolved metric descriptor: the final
/// metric name (prefix, name and unit) plus the merged const labels.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Descriptor<'a> {
    prefix: Option<&'a Prefix>,
    name: &'a str,
    unit: Option<&'a Unit>,
    const_labels: ConstLabels<'a>,
}

impl<'a> Descriptor<'a> {
    pub(crate) fn prefix(&self) -> Option<&'a Prefix> {
        self.prefix
    }

    pub(crate) fn name(&self) -> &'a str {
        self.name
    }

    pub(crate) fn unit(&self) -> Option<&'a Unit> {
        self.unit
    }

    pub(crate) fn const_labels(&self) -> ConstLabels<'a> {
        self.const_labels
    }

    /// Write `<prefix>_<name>` without allocating.
    pub(crate) fn write_prefix_name(&self, writer: &mut dyn Write) -> Result<(), std::fmt::Error> {
        if let Some(prefix) = self.prefix {
            writer.write_str(prefix.as_str())?;
            writer.write_str("_")?;
        }
        writer.write_str(self.name)
    }

    /// Write the `_<unit>` suffix, if a unit is set, without allocating.
    pub(crate) fn write_unit(&self, writer: &mut dyn Write) -> Result<(), std::fmt::Error> {
        if let Some(unit) = self.unit {
            writer.write_str("_")?;
            writer.write_str(unit.as_str())?;
        }
        Ok(())
    }

    /// Write the final metric name `<prefix>_<name>_<unit>` without
    /// allocating.
    pub(crate) fn write_name(&self, writer: &mut dyn Write) -> Result<(), std::fmt::Error> {
        self.write_prefix_name(writer)?;
        self.write_unit(writer)
    }

    /// The final metric name `<prefix>_<name>_<unit>` as an owned [`String`].
    ///
    /// Used by the protobuf encoders, whose data model stores the family name
    /// as an owned string.
    #[cfg(feature = "prometheus_protobuf")]
    pub(crate) fn name_string(&self) -> String {
        let mut name = String::new();
        self.push_prefix_name(&mut name);
        if let Some(unit) = self.unit {
            name.push('_');
            name.push_str(unit.as_str());
        }
        name
    }

    /// The metric name `<prefix>_<name>` without the unit suffix.
    ///
    /// Used by the OpenMetrics protobuf encoder, which carries the unit in a
    /// dedicated message field instead of the family name.
    #[cfg(feature = "openmetrics_protobuf")]
    pub(crate) fn prefix_name_string(&self) -> String {
        let mut name = String::new();
        self.push_prefix_name(&mut name);
        name
    }

    #[cfg(any(feature = "prometheus_protobuf", feature = "openmetrics_protobuf"))]
    fn push_prefix_name(&self, name: &mut String) {
        if let Some(prefix) = self.prefix {
            name.push_str(prefix.as_str());
            name.push('_');
        }
        name.push_str(self.name);
    }
}

/// Const labels of a (sub-)registry in stable registration order.
///
/// This is a borrowed view; traversing it never copies the label set.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ConstLabels<'a>(&'a [(Cow<'static, str>, Cow<'static, str>)]);

impl<'a> ConstLabels<'a> {
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn as_slice(&self) -> &'a [(Cow<'static, str>, Cow<'static, str>)] {
        self.0
    }
}

/// Stable merge order of the three label groups of a metric: const labels
/// first, then per-sample additional labels, then family labels.
///
/// The determination is purely presence based (a group that is present but
/// encodes to nothing still counts), matching the historic encoder behavior.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LabelOrder {
    has_const: bool,
    has_additional: bool,
    has_family: bool,
}

impl LabelOrder {
    pub(crate) fn new(
        const_labels: ConstLabels<'_>,
        has_additional: bool,
        has_family: bool,
    ) -> Self {
        Self {
            has_const: !const_labels.is_empty(),
            has_additional,
            has_family,
        }
    }

    /// Whether no label group is present at all.
    pub(crate) fn is_empty(&self) -> bool {
        !self.has_const && !self.has_additional && !self.has_family
    }

    /// Whether a separator is needed between the const labels and the
    /// additional labels.
    pub(crate) fn separate_after_const(&self) -> bool {
        self.has_const && self.has_additional
    }

    /// Whether the family labels need to be separated from preceding labels.
    pub(crate) fn separate_before_family(&self) -> bool {
        self.has_const || self.has_additional
    }
}

#[cfg(test)]
mod tests {
    use super::{ConstLabels, LabelOrder, Scope};
    use crate::encoding::{EncodeLabel, EncodeLabelSet, LabelSetEncoder};
    use crate::metrics::counter::Counter;
    use crate::metrics::family::Family;
    use crate::metrics::gauge::Gauge;
    use crate::metrics::histogram::{Histogram, NativeHistogramConfig};
    use crate::registry::{Prefix, Registry, Unit};
    use std::borrow::Cow;
    use std::fmt::Write as _;

    #[test]
    fn descriptor_core_name_composition() {
        let prefix = Prefix::from("my_prefix".to_string());
        let labels = vec![(Cow::Borrowed("env"), Cow::Borrowed("prod"))];
        let scope = Scope::new(Some(&prefix), &labels);

        let descriptor = scope.descriptor("my_counter", Some(&Unit::Seconds));

        let mut name = String::new();
        descriptor.write_name(&mut name).unwrap();
        assert_eq!("my_prefix_my_counter_seconds", name);

        let descriptor = scope.descriptor("my_counter", None);
        let mut name = String::new();
        descriptor.write_name(&mut name).unwrap();
        assert_eq!("my_prefix_my_counter", name);

        let root = Scope::default().descriptor("my_counter", None);
        let mut name = String::new();
        root.write_name(&mut name).unwrap();
        assert_eq!("my_counter", name);
        assert_eq!(
            vec![("env", "prod")],
            descriptor
                .const_labels()
                .as_slice()
                .iter()
                .map(|(key, value)| (key.as_ref(), value.as_ref()))
                .collect::<Vec<_>>()
        );
    }

    #[cfg(feature = "prometheus_protobuf")]
    #[test]
    fn descriptor_core_owned_name_composition() {
        let prefix = Prefix::from("my_prefix".to_string());
        let labels = vec![(Cow::Borrowed("env"), Cow::Borrowed("prod"))];
        let scope = Scope::new(Some(&prefix), &labels);

        let descriptor = scope.descriptor("my_counter", Some(&Unit::Seconds));
        assert_eq!("my_prefix_my_counter_seconds", descriptor.name_string());

        let descriptor = scope.descriptor("my_counter", None);
        assert_eq!("my_prefix_my_counter", descriptor.name_string());

        let root = Scope::default().descriptor("my_counter", None);
        assert_eq!("my_counter", root.name_string());
    }

    #[cfg(feature = "openmetrics_protobuf")]
    #[test]
    fn descriptor_core_prefix_name_composition() {
        let prefix = Prefix::from("my_prefix".to_string());
        let labels = vec![(Cow::Borrowed("env"), Cow::Borrowed("prod"))];
        let scope = Scope::new(Some(&prefix), &labels);

        let descriptor = scope.descriptor("my_counter", Some(&Unit::Seconds));
        assert_eq!("my_prefix_my_counter", descriptor.prefix_name_string());
    }

    #[test]
    fn descriptor_core_label_order() {
        let empty = ConstLabels::default();
        assert!(LabelOrder::new(empty, false, false).is_empty());
        assert!(!LabelOrder::new(empty, true, false).is_empty());
        assert!(!LabelOrder::new(empty, false, true).is_empty());

        let labels = [(Cow::Borrowed("env"), Cow::Borrowed("prod"))];
        let scope = Scope::new(None, &labels);
        let descriptor = scope.descriptor("my_counter", None);
        let const_labels = descriptor.const_labels();

        let order = LabelOrder::new(const_labels, true, true);
        assert!(order.separate_after_const());
        assert!(order.separate_before_family());

        let order = LabelOrder::new(const_labels, false, true);
        assert!(!order.separate_after_const());
        assert!(order.separate_before_family());

        let order = LabelOrder::new(const_labels, true, false);
        assert!(order.separate_after_const());
        assert!(order.separate_before_family());

        let order = LabelOrder::new(ConstLabels::default(), false, true);
        assert!(!order.separate_after_const());
        assert!(!order.separate_before_family());
    }

    /// A label set that encodes one label and then fails midway.
    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    struct FailingLabelSet;

    impl EncodeLabelSet for FailingLabelSet {
        fn encode(&self, encoder: &mut LabelSetEncoder) -> Result<(), std::fmt::Error> {
            ("ok", "1").encode(encoder.encode_label())?;
            let mut label = encoder.encode_label();
            let mut key = label.encode_label_key()?;
            key.write_str("boom")?;
            Err(std::fmt::Error)
        }
    }

    fn failing_label_registry() -> (Registry, Family<FailingLabelSet, Counter>) {
        let mut registry = Registry::default();
        let family = Family::<FailingLabelSet, Counter>::default();
        registry.register("my_counter", "My counter", family.clone());
        family.get_or_create(&FailingLabelSet).inc();
        (registry, family)
    }

    #[test]
    fn descriptor_core_prefix_and_unit_text() {
        let mut registry = Registry::with_prefix("my_prefix");
        let counter: Counter = Counter::default();
        registry.register_with_unit("my_counter", "My counter", Unit::Seconds, counter.clone());
        counter.inc();

        let mut encoded = String::new();
        text::encode(&mut encoded, &registry).unwrap();

        let expected = "# HELP my_prefix_my_counter_seconds My counter.\n".to_owned()
            + "# TYPE my_prefix_my_counter_seconds counter\n"
            + "# UNIT my_prefix_my_counter_seconds seconds\n"
            + "my_prefix_my_counter_seconds_total 1\n"
            + "# EOF\n";
        assert_eq!(expected, encoded);
    }

    #[test]
    fn descriptor_core_parent_child_same_name_label_text() {
        let mut registry = Registry::default();
        let sub = registry.sub_registry_with_label((Cow::Borrowed("env"), Cow::Borrowed("prod")));
        let sub_sub =
            sub.sub_registry_with_label((Cow::Borrowed("env"), Cow::Borrowed("dev")));
        let gauge: Gauge = Gauge::default();
        sub_sub.register("my_gauge", "My gauge", gauge);

        let mut encoded = String::new();
        text::encode(&mut encoded, &registry).unwrap();

        let expected = "# HELP my_gauge My gauge.\n".to_owned()
            + "# TYPE my_gauge gauge\n"
            + "my_gauge{env=\"prod\",env=\"dev\"} 0\n"
            + "# EOF\n";
        assert_eq!(expected, encoded);
    }

    #[test]
    fn descriptor_core_empty_label_set_text() {
        let mut registry = Registry::default();
        let counter: Counter = Counter::default();
        registry.register("my_counter", "My counter", counter);

        let mut encoded = String::new();
        text::encode(&mut encoded, &registry).unwrap();

        let expected = "# HELP my_counter My counter.\n".to_owned()
            + "# TYPE my_counter counter\n"
            + "my_counter_total 0\n"
            + "# EOF\n";
        assert_eq!(expected, encoded);
    }

    #[test]
    fn descriptor_core_help_with_backslash_and_newline_text() {
        let mut registry = Registry::default();
        let counter: Counter = Counter::default();
        registry.register("my_counter", "first line\nsecond line \\ suffix", counter);

        let mut encoded = String::new();
        text::encode(&mut encoded, &registry).unwrap();

        let expected = "# HELP my_counter first line\nsecond line \\ suffix.\n".to_owned()
            + "# TYPE my_counter counter\n"
            + "my_counter_total 0\n"
            + "# EOF\n";
        assert_eq!(expected, encoded);
    }

    #[test]
    fn descriptor_core_label_encoder_fmt_error_text() {
        let (registry, _family) = failing_label_registry();

        let mut encoded = String::new();
        assert!(text::encode(&mut encoded, &registry).is_err());
    }

    #[test]
    fn descriptor_core_native_histogram_text() {
        let mut registry = Registry::default();
        let histogram =
            Histogram::new_classic_and_native([1.0, 2.0], NativeHistogramConfig::with_schema(0));
        registry.register("my_histogram", "My histogram", histogram.clone());
        histogram.observe(1.0);

        let mut encoded = String::new();
        text::encode(&mut encoded, &registry).unwrap();

        let expected = "# HELP my_histogram My histogram.\n".to_owned()
            + "# TYPE my_histogram histogram\n"
            + "my_histogram_sum 1.0\n"
            + "my_histogram_count 1\n"
            + "my_histogram_bucket{le=\"1.0\"} 1\n"
            + "my_histogram_bucket{le=\"2.0\"} 1\n"
            + "my_histogram_bucket{le=\"+Inf\"} 1\n"
            + "# EOF\n";
        assert_eq!(expected, encoded);
    }

    #[test]
    fn descriptor_core_empty_family_text() {
        let mut registry = Registry::default();
        let family = Family::<Vec<(String, String)>, Counter>::default();
        registry.register("my_counter_family", "My counter family", family);

        let mut encoded = String::new();
        text::encode(&mut encoded, &registry).unwrap();

        assert_eq!("# EOF\n", encoded);
    }

    use crate::encoding::text;

    #[cfg(feature = "prometheus_protobuf")]
    mod protobuf {
        use super::*;
        use crate::encoding::prometheus_protobuf::{
            encode, encode_to_vec, prometheus_data_model,
        };
        use prost::Message;

        fn expected_bytes(family: &prometheus_data_model::MetricFamily) -> Vec<u8> {
            let mut bytes = Vec::new();
            family.encode_length_delimited(&mut bytes).unwrap();
            bytes
        }

        #[test]
        fn descriptor_core_prefix_and_unit_protobuf() {
            let mut registry = Registry::with_prefix("my_prefix");
            let counter: Counter = Counter::default();
            registry.register_with_unit(
                "my_counter",
                "My counter",
                Unit::Seconds,
                counter.clone(),
            );
            counter.inc();

            let expected = prometheus_data_model::MetricFamily {
                name: "my_prefix_my_counter_seconds_total".to_string(),
                r#type: prometheus_data_model::MetricType::Counter as i32,
                unit: "seconds".to_string(),
                help: "My counter.".to_string(),
                metric: vec![prometheus_data_model::Metric {
                    counter: Some(prometheus_data_model::Counter {
                        value: 1.0,
                        exemplar: None,
                        start_timestamp: None,
                    }),
                    ..Default::default()
                }],
            };

            assert_eq!(vec![expected.clone()], encode(&registry).unwrap());
            assert_eq!(expected_bytes(&expected), encode_to_vec(&registry).unwrap());
        }

        #[test]
        fn descriptor_core_parent_child_same_name_label_protobuf() {
            let mut registry = Registry::default();
            let sub = registry
                .sub_registry_with_label((Cow::Borrowed("env"), Cow::Borrowed("prod")));
            let sub_sub =
                sub.sub_registry_with_label((Cow::Borrowed("env"), Cow::Borrowed("dev")));
            let gauge: Gauge = Gauge::default();
            sub_sub.register("my_gauge", "My gauge", gauge);

            let expected = prometheus_data_model::MetricFamily {
                name: "my_gauge".to_string(),
                r#type: prometheus_data_model::MetricType::Gauge as i32,
                unit: String::new(),
                help: "My gauge.".to_string(),
                metric: vec![prometheus_data_model::Metric {
                    label: vec![
                        prometheus_data_model::LabelPair {
                            name: "env".to_string(),
                            value: "prod".to_string(),
                        },
                        prometheus_data_model::LabelPair {
                            name: "env".to_string(),
                            value: "dev".to_string(),
                        },
                    ],
                    gauge: Some(prometheus_data_model::Gauge { value: 0.0 }),
                    ..Default::default()
                }],
            };

            assert_eq!(vec![expected.clone()], encode(&registry).unwrap());
            assert_eq!(expected_bytes(&expected), encode_to_vec(&registry).unwrap());
        }

        #[test]
        fn descriptor_core_empty_label_set_protobuf() {
            let mut registry = Registry::default();
            let counter: Counter = Counter::default();
            registry.register("my_counter", "My counter", counter);

            let expected = prometheus_data_model::MetricFamily {
                name: "my_counter_total".to_string(),
                r#type: prometheus_data_model::MetricType::Counter as i32,
                unit: String::new(),
                help: "My counter.".to_string(),
                metric: vec![prometheus_data_model::Metric {
                    counter: Some(prometheus_data_model::Counter {
                        value: 0.0,
                        exemplar: None,
                        start_timestamp: None,
                    }),
                    ..Default::default()
                }],
            };

            assert_eq!(vec![expected.clone()], encode(&registry).unwrap());
            assert_eq!(expected_bytes(&expected), encode_to_vec(&registry).unwrap());
        }

        #[test]
        fn descriptor_core_help_with_backslash_and_newline_protobuf() {
            let mut registry = Registry::default();
            let counter: Counter = Counter::default();
            registry.register("my_counter", "first line\nsecond line \\ suffix", counter);

            let families = encode(&registry).unwrap();
            assert_eq!(
                "first line\nsecond line \\ suffix.",
                families[0].help,
                "help text must be carried over unescaped"
            );
        }

        #[test]
        fn descriptor_core_label_encoder_fmt_error_protobuf() {
            let (registry, _family) = failing_label_registry();

            assert!(encode(&registry).is_err());
        }

        #[test]
        fn descriptor_core_native_histogram_protobuf() {
            let mut registry = Registry::default();
            let histogram = Histogram::new_native(NativeHistogramConfig::with_schema(0));
            registry.register("my_histogram", "My histogram", histogram.clone());
            histogram.observe(1.0);
            histogram.observe(4.0);
            histogram.observe(-2.0);

            let families = encode(&registry).unwrap();
            assert_eq!("my_histogram", families[0].name);
            assert_eq!(
                prometheus_data_model::MetricType::Histogram as i32,
                families[0].r#type
            );

            let histogram = families[0].metric[0].histogram.as_ref().unwrap();
            assert_eq!(3, histogram.sample_count);
            assert_eq!(3.0, histogram.sample_sum);
            assert_eq!(0, histogram.schema);
            assert_eq!(
                vec![prometheus_data_model::BucketSpan {
                    offset: 1,
                    length: 1
                }],
                histogram.negative_span
            );
            assert_eq!(vec![1], histogram.negative_delta);
            assert_eq!(
                vec![prometheus_data_model::BucketSpan {
                    offset: 0,
                    length: 3
                }],
                histogram.positive_span
            );
            assert_eq!(vec![1, -1, 1], histogram.positive_delta);
            assert_eq!(0, histogram.zero_count);
            assert!(histogram.bucket.is_empty());
            assert!(histogram.start_timestamp.is_some());
        }

        #[test]
        fn descriptor_core_empty_family_protobuf() {
            let mut registry = Registry::default();
            let family = Family::<Vec<(String, String)>, Counter>::default();
            registry.register("my_counter_family", "My counter family", family);

            assert!(encode(&registry).unwrap().is_empty());
            assert!(encode_to_vec(&registry).unwrap().is_empty());
        }

        /// Shared probe: both formats must observe the registry's descriptors
        /// in the same stable order.
        #[test]
        fn descriptor_core_shared_probe_order() {
            use crate::collector::Collector;
            use crate::encoding::{DescriptorEncoder, EncodeMetric};
            use crate::metrics::counter::ConstCounter;

            #[derive(Debug)]
            struct MyCollector;

            impl Collector for MyCollector {
                fn encode(
                    &self,
                    mut encoder: DescriptorEncoder,
                ) -> Result<(), std::fmt::Error> {
                    let counter = ConstCounter::new(1u64);
                    let metric_encoder = encoder.encode_descriptor(
                        "collected",
                        "Collected metric",
                        None,
                        counter.metric_type(),
                    )?;
                    counter.encode(metric_encoder)
                }
            }

            let mut registry = Registry::with_prefix("app");

            let counter: Counter = Counter::default();
            registry.register("requests", "Total requests", counter.clone());
            counter.inc();

            registry.register_collector(Box::new(MyCollector));

            let sub = registry.sub_registry_with_prefix("db");
            let gauge: Gauge = Gauge::default();
            sub.register("connections", "Open connections", gauge);

            let sub_sub =
                sub.sub_registry_with_label((Cow::Borrowed("zone"), Cow::Borrowed("a")));
            let family = Family::<Vec<(String, String)>, Counter>::default();
            sub_sub.register("queries", "Queries", family.clone());
            family
                .get_or_create(&vec![("handler".to_string(), "/api".to_string())])
                .inc();

            // Descriptor order as seen by the text encoder.
            let mut text_out = String::new();
            text::encode(&mut text_out, &registry).unwrap();
            let text_order: Vec<&str> = text_out
                .lines()
                .filter_map(|line| {
                    line.strip_prefix("# HELP ")
                        .map(|rest| rest.split(' ').next().unwrap())
                })
                .collect();

            // Descriptor order as seen by the protobuf encoder, normalized by
            // stripping the type suffixes the protobuf format folds into the
            // family name.
            let proto_order: Vec<String> = encode(&registry)
                .unwrap()
                .iter()
                .map(|family| {
                    family
                        .name
                        .strip_suffix("_total")
                        .or_else(|| family.name.strip_suffix("_info"))
                        .unwrap_or(&family.name)
                        .to_string()
                })
                .collect();

            println!("descriptor_core stable descriptor order:");
            for (text_name, proto_name) in text_order.iter().zip(proto_order.iter()) {
                println!("  text={text_name} protobuf={proto_name}");
            }

            let expected = vec![
                "app_requests".to_string(),
                "app_collected".to_string(),
                "app_db_connections".to_string(),
                "app_db_queries".to_string(),
            ];
            assert_eq!(expected, text_order);
            assert_eq!(expected, proto_order);
        }
    }
}
