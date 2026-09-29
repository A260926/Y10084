//! Behavior guards pinning the writer-independent descriptor and label
//! traversal core shared by the text and protobuf encoders.
//!
//! Every guard asserts the exact bytes of each supported format as well as a
//! shared probe proving both formats visit descriptors in the same stable
//! order.

use std::borrow::Cow;
use std::fmt;

use prometheus_client::encoding::text::encode as encode_text;
use prometheus_client::encoding::{
    EncodeLabelKey, EncodeLabelSet, EncodeLabelValue, EncodeMetric, LabelSetEncoder, MetricEncoder,
};
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::histogram::{
    exponential_buckets, Histogram, NativeHistogramConfig,
};
use prometheus_client::metrics::MetricType;
use prometheus_client::registry::{Registry, Unit};

#[test]
fn descriptor_core_text_prefix_and_unit_combination() {
    let mut registry = Registry::default();
    let subsystem = registry.sub_registry_with_prefix("sub");
    let counter: Counter = Counter::default();
    subsystem.register_with_unit("requests", "Subsystem requests", Unit::Seconds, counter);

    let mut encoded = String::new();
    encode_text(&mut encoded, &registry).unwrap();

    assert_eq!(
        encoded,
        concat!(
            "# HELP sub_requests_seconds Subsystem requests.\n",
            "# TYPE sub_requests_seconds counter\n",
            "# UNIT sub_requests_seconds seconds\n",
            "sub_requests_seconds_total 0\n",
            "# EOF\n",
        )
    );
}

#[test]
fn descriptor_core_text_parent_child_same_named_labels() {
    let mut registry = Registry::default();
    let parent = registry.sub_registry_with_label((Cow::Borrowed("env"), Cow::Borrowed("parent")));
    let child = parent.sub_registry_with_label((Cow::Borrowed("env"), Cow::Borrowed("child")));
    let gauge: Gauge = Gauge::default();
    gauge.set(7);
    child.register("same_label_gauge", "Gauge with same named labels", gauge);

    let mut encoded = String::new();
    encode_text(&mut encoded, &registry).unwrap();

    assert_eq!(
        encoded,
        concat!(
            "# HELP same_label_gauge Gauge with same named labels.\n",
            "# TYPE same_label_gauge gauge\n",
            "same_label_gauge{env=\"parent\",env=\"child\"} 7\n",
            "# EOF\n",
        )
    );
}

#[test]
fn descriptor_core_text_empty_label_set_and_empty_family_emit_nothing() {
    let mut registry = Registry::default();

    // Registered but with no members: family is empty and must be skipped.
    let empty_family = Family::<Vec<(String, String)>, Counter>::default();
    registry.register("empty_family", "Empty family", empty_family);

    let counter: Counter = Counter::default();
    registry.register("solo_counter", "Solo counter", counter);

    let mut encoded = String::new();
    encode_text(&mut encoded, &registry).unwrap();

    assert_eq!(
        encoded,
        concat!(
            "# HELP solo_counter Solo counter.\n",
            "# TYPE solo_counter counter\n",
            "solo_counter_total 0\n",
            "# EOF\n",
        )
    );
}

#[test]
fn descriptor_core_text_help_with_backslash_and_newline_is_verbatim() {
    let mut registry = Registry::default();
    let gauge: Gauge = Gauge::default();
    gauge.set(1);
    registry.register("escape_help", "line one\\line two\nsecond line", gauge);

    let mut encoded = String::new();
    encode_text(&mut encoded, &registry).unwrap();

    assert_eq!(
        encoded,
        concat!(
            "# HELP escape_help line one\\line two\nsecond line.\n",
            "# TYPE escape_help gauge\n",
            "escape_help 1\n",
            "# EOF\n",
        )
    );
}

/// Label value encoder that always fails, verifying error propagation through
/// the shared label pipeline.
struct FailingValue;

impl EncodeLabelValue for FailingValue {
    fn encode(
        &self,
        _encoder: &mut prometheus_client::encoding::LabelValueEncoder,
    ) -> Result<(), fmt::Error> {
        Err(fmt::Error)
    }
}

/// Label set emitting one fine label followed by one failing label.
#[derive(Clone, Debug)]
struct FlakyLabels {
    ok: String,
}

impl EncodeLabelSet for FlakyLabels {
    fn encode(&self, encoder: &mut LabelSetEncoder) -> Result<(), fmt::Error> {
        let mut label = encoder.encode_label();
        let mut key = label.encode_label_key()?;
        EncodeLabelKey::encode(&"ok", &mut key)?;
        let mut value = key.encode_label_value()?;
        EncodeLabelValue::encode(&self.ok, &mut value)?;
        value.finish()?;

        let mut label = encoder.encode_label();
        let mut key = label.encode_label_key()?;
        EncodeLabelKey::encode(&"boom", &mut key)?;
        let mut value = key.encode_label_value()?;
        FailingValue.encode(&mut value)
    }
}

#[derive(Debug)]
struct FlakyMetric {
    labels: FlakyLabels,
}

impl EncodeMetric for FlakyMetric {
    fn encode(&self, mut encoder: MetricEncoder) -> Result<(), fmt::Error> {
        let mut encoder = encoder.encode_family(&self.labels)?;
        encoder.encode_gauge(&1_i64)
    }

    fn metric_type(&self) -> MetricType {
        MetricType::Gauge
    }
}

#[test]
fn descriptor_core_text_label_encoder_error_is_propagated() {
    let mut registry = Registry::default();
    registry.register(
        "flaky",
        "Flaky metric",
        FlakyMetric {
            labels: FlakyLabels {
                ok: "fine".to_owned(),
            },
        },
    );

    let mut encoded = String::new();
    assert!(encode_text(&mut encoded, &registry).is_err());
    // Descriptor lines precede the failing sample and stay intact.
    assert!(encoded.starts_with(concat!(
        "# HELP flaky Flaky metric.\n",
        "# TYPE flaky gauge\n",
    )));
}

#[test]
fn descriptor_core_text_native_only_histogram_errors() {
    let mut registry = Registry::default();
    let histogram = Histogram::new_native(NativeHistogramConfig::with_schema(0));
    registry.register("native_histogram", "Native histogram", histogram.clone());
    histogram.observe(1.0);

    let mut encoded = String::new();
    assert!(encode_text(&mut encoded, &registry).is_err());
}

#[test]
fn descriptor_core_text_native_histogram_emits_classic_fallback() {
    let mut registry = Registry::default();
    let histogram = Histogram::new_classic_and_native(
        exponential_buckets(1.0, 2.0, 2),
        NativeHistogramConfig::with_schema(0),
    );
    registry.register("hybrid_histogram", "Hybrid histogram", histogram.clone());
    histogram.observe(1.0);
    histogram.observe(4.0);

    let mut encoded = String::new();
    encode_text(&mut encoded, &registry).unwrap();

    assert_eq!(
        encoded,
        concat!(
            "# HELP hybrid_histogram Hybrid histogram.\n",
            "# TYPE hybrid_histogram histogram\n",
            "hybrid_histogram_sum 5.0\n",
            "hybrid_histogram_count 2\n",
            "hybrid_histogram_bucket{le=\"1.0\"} 1\n",
            "hybrid_histogram_bucket{le=\"2.0\"} 1\n",
            "hybrid_histogram_bucket{le=\"+Inf\"} 2\n",
            "# EOF\n",
        )
    );
}

/// Registry shared by the order probe and the exact-byte guards.
fn build_ordered_registry() -> Registry {
    let mut registry = Registry::default();

    let alpha: Counter = Counter::default();
    registry.register("alpha", "Alpha metric", alpha);

    let sub = registry.sub_registry_with_prefix("sub");
    let sub_counter: Counter = Counter::default();
    sub.register_with_unit("beta", "Beta metric", Unit::Bytes, sub_counter);

    let sub_child = sub.sub_registry_with_label((Cow::Borrowed("env"), Cow::Borrowed("child")));
    let gauge: Gauge = Gauge::default();
    gauge.set(3);
    sub_child.register("gamma", "Gamma metric", gauge);

    let root_gauge: Gauge = Gauge::default();
    root_gauge.set(1);
    registry.register("delta", "Delta metric", root_gauge);

    registry
}

/// Stable descriptor visitation order: registration order, depth first.
const EXPECTED_DESCRIPTOR_ORDER: &[&str] = &["alpha", "delta", "sub_beta_bytes", "sub_gamma"];

#[test]
fn descriptor_core_text_visits_stable_descriptor_order() {
    let registry = build_ordered_registry();

    let mut encoded = String::new();
    encode_text(&mut encoded, &registry).unwrap();

    println!("stable descriptor order (text):");
    for descriptor in EXPECTED_DESCRIPTOR_ORDER {
        println!("{descriptor}");
    }

    let help_names: Vec<&str> = encoded
        .lines()
        .filter_map(|line| line.strip_prefix("# HELP "))
        .map(|line| line.split(' ').next().unwrap())
        .collect();
    assert_eq!(help_names, EXPECTED_DESCRIPTOR_ORDER);
}

// ---------------------------------------------------------------------------
// Prometheus protobuf guards.
//
// Exact bytes are verified through the public length-delimited encoder against
// hand-constructed expected messages, keeping the probe independent of the
// internals under refactor.
// ---------------------------------------------------------------------------

#[cfg(feature = "prometheus_protobuf")]
mod prometheus {
    use super::*;
    use prometheus_client::encoding::prometheus_protobuf::{
        encode as encode_prometheus, prometheus_data_model as dm,
    };
    use prost::Message;

    #[test]
    fn descriptor_core_prometheus_prefix_and_unit_combination() {
        let mut registry = Registry::default();
        let subsystem = registry.sub_registry_with_prefix("sub");
        let counter: Counter = Counter::default();
        counter.inc();
        subsystem.register_with_unit("requests", "Subsystem requests", Unit::Seconds, counter);

        let families = encode_prometheus(&registry).unwrap();
        let expected = vec![dm::MetricFamily {
            name: "sub_requests_seconds_total".to_owned(),
            help: "Subsystem requests.".to_owned(),
            r#type: dm::MetricType::Counter as i32,
            unit: "seconds".to_owned(),
            metric: vec![dm::Metric {
                counter: Some(dm::Counter {
                    value: 1.0,
                    ..Default::default()
                }),
                ..Default::default()
            }],
        }];

        assert_eq!(families, expected);
        assert_eq!(encode_vec(&families), encode_vec(&expected));
    }

    #[test]
    fn descriptor_core_prometheus_parent_child_same_named_labels() {
        let mut registry = Registry::default();
        let parent =
            registry.sub_registry_with_label((Cow::Borrowed("env"), Cow::Borrowed("parent")));
        let child = parent.sub_registry_with_label((Cow::Borrowed("env"), Cow::Borrowed("child")));
        let gauge: Gauge = Gauge::default();
        gauge.set(7);
        child.register("same_label_gauge", "Gauge with same named labels", gauge);

        let families = encode_prometheus(&registry).unwrap();
        let expected = vec![dm::MetricFamily {
            name: "same_label_gauge".to_owned(),
            help: "Gauge with same named labels.".to_owned(),
            r#type: dm::MetricType::Gauge as i32,
            unit: String::new(),
            metric: vec![dm::Metric {
                label: vec![
                    dm::LabelPair {
                        name: "env".to_owned(),
                        value: "parent".to_owned(),
                    },
                    dm::LabelPair {
                        name: "env".to_owned(),
                        value: "child".to_owned(),
                    },
                ],
                gauge: Some(dm::Gauge { value: 7.0 }),
                ..Default::default()
            }],
        }];

        assert_eq!(families, expected);
        assert_eq!(encode_vec(&families), encode_vec(&expected));
    }

    #[test]
    fn descriptor_core_prometheus_help_with_backslash_and_newline_is_verbatim() {
        let mut registry = Registry::default();
        let gauge: Gauge = Gauge::default();
        gauge.set(1);
        registry.register("escape_help", "line one\\line two\nsecond line", gauge);

        let families = encode_prometheus(&registry).unwrap();
        let expected = vec![dm::MetricFamily {
            name: "escape_help".to_owned(),
            help: "line one\\line two\nsecond line.".to_owned(),
            r#type: dm::MetricType::Gauge as i32,
            unit: String::new(),
            metric: vec![dm::Metric {
                gauge: Some(dm::Gauge { value: 1.0 }),
                ..Default::default()
            }],
        }];

        assert_eq!(families, expected);
        assert_eq!(encode_vec(&families), encode_vec(&expected));
    }

    #[test]
    fn descriptor_core_prometheus_label_encoder_error_is_propagated() {
        let mut registry = Registry::default();
        registry.register(
            "flaky",
            "Flaky metric",
            super::FlakyMetric {
                labels: super::FlakyLabels {
                    ok: "fine".to_owned(),
                },
            },
        );

        assert!(encode_prometheus(&registry).is_err());
    }

    #[test]
    fn descriptor_core_prometheus_empty_family_is_skipped() {
        let mut registry = Registry::default();
        let empty_family = Family::<Vec<(String, String)>, Counter>::default();
        registry.register("empty_family", "Empty family", empty_family);
        let counter: Counter = Counter::default();
        counter.inc();
        registry.register("solo_counter", "Solo counter", counter);

        let families = encode_prometheus(&registry).unwrap();
        let expected = vec![dm::MetricFamily {
            name: "solo_counter_total".to_owned(),
            help: "Solo counter.".to_owned(),
            r#type: dm::MetricType::Counter as i32,
            unit: String::new(),
            metric: vec![dm::Metric {
                counter: Some(dm::Counter {
                    value: 1.0,
                    ..Default::default()
                }),
                ..Default::default()
            }],
        }];

        assert_eq!(families, expected);
        assert_eq!(encode_vec(&families), encode_vec(&expected));
    }

    #[test]
    fn descriptor_core_prometheus_native_histogram() {
        let mut registry = Registry::default();
        let histogram = Histogram::new_native(NativeHistogramConfig::with_schema(0));
        registry.register("my_histogram", "My histogram", histogram.clone());
        histogram.observe(1.0);
        histogram.observe(4.0);
        histogram.observe(-2.0);

        let families = encode_prometheus(&registry).unwrap();
        assert_eq!(families.len(), 1);
        assert_eq!(families[0].name, "my_histogram");
        assert_eq!(families[0].r#type, dm::MetricType::Histogram as i32);

        let histogram = families[0].metric[0].histogram.as_ref().unwrap();
        assert_eq!(histogram.sample_count, 3);
        assert_eq!(histogram.sample_sum, 3.0);
        assert_eq!(histogram.schema, 0);
        assert_eq!(histogram.negative_span.len(), 1);
        assert_eq!(histogram.positive_span.len(), 1);
        assert_eq!(histogram.negative_delta, vec![1]);
        assert_eq!(histogram.positive_delta, vec![1, -1, 1]);
        assert!(histogram.bucket.is_empty());
        assert!(histogram.start_timestamp.is_some());

        // The exact bytes are deterministic given the model; assert the family
        // round-trips through the wire format byte-for-byte.
        let encoded = encode_vec(&families);
        let mut decoded = Vec::new();
        let mut rest = encoded.as_slice();
        while !rest.is_empty() {
            decoded.push(dm::MetricFamily::decode_length_delimited(&mut rest).unwrap());
        }
        assert_eq!(decoded, families);
        assert_eq!(encode_vec(&decoded), encoded);
    }

    #[test]
    fn descriptor_core_prometheus_visits_stable_descriptor_order() {
        let registry = build_ordered_registry();

        let families = encode_prometheus(&registry).unwrap();
        let names: Vec<String> = families.iter().map(|family| family.name.clone()).collect();

        // Prometheus appends the `_total` suffix to counters; strip it to
        // recover the shared base-name order.
        let base_names: Vec<&str> = names
            .iter()
            .map(|name| name.strip_suffix("_total").unwrap_or(name.as_str()))
            .collect();
        assert_eq!(base_names, EXPECTED_DESCRIPTOR_ORDER);
    }

    fn encode_vec(families: &[dm::MetricFamily]) -> Vec<u8> {
        let mut encoded = Vec::new();
        for family in families {
            family.encode_length_delimited(&mut encoded).unwrap();
        }
        encoded
    }
}

// ---------------------------------------------------------------------------
// OpenMetrics protobuf guards.
// ---------------------------------------------------------------------------

#[cfg(feature = "openmetrics_protobuf")]
#[allow(deprecated)]
mod openmetrics {
    use super::*;
    use prometheus_client::encoding::openmetrics_protobuf::{
        encode as encode_openmetrics, openmetrics_data_model as dm,
    };

    #[test]
    fn descriptor_core_openmetrics_prefix_and_unit_combination() {
        let mut registry = Registry::default();
        let subsystem = registry.sub_registry_with_prefix("sub");
        let counter: Counter = Counter::default();
        counter.inc();
        subsystem.register_with_unit("requests", "Subsystem requests", Unit::Seconds, counter);

        let metric_set = encode_openmetrics(&registry).unwrap();
        let families = metric_set.metric_families;
        let expected = vec![dm::MetricFamily {
            // OpenMetrics protobuf keeps the family name free of the unit and
            // the `_total` suffix.
            name: "sub_requests".to_owned(),
            help: "Subsystem requests.".to_owned(),
            r#type: dm::MetricType::Counter as i32,
            unit: "seconds".to_owned(),
            metrics: vec![dm::Metric {
                metric_points: vec![dm::MetricPoint {
                    value: Some(dm::metric_point::Value::CounterValue(dm::CounterValue {
                        total: Some(dm::counter_value::Total::IntValue(1)),
                        ..Default::default()
                    })),
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }];

        assert_eq!(families, expected);
    }

    #[test]
    fn descriptor_core_openmetrics_parent_child_same_named_labels() {
        let mut registry = Registry::default();
        let parent =
            registry.sub_registry_with_label((Cow::Borrowed("env"), Cow::Borrowed("parent")));
        let child = parent.sub_registry_with_label((Cow::Borrowed("env"), Cow::Borrowed("child")));
        let gauge: Gauge = Gauge::default();
        gauge.set(7);
        child.register("same_label_gauge", "Gauge with same named labels", gauge);

        let metric_set = encode_openmetrics(&registry).unwrap();
        let families = metric_set.metric_families;
        assert_eq!(families.len(), 1);
        assert_eq!(families[0].name, "same_label_gauge");
        assert_eq!(
            families[0].metrics[0].labels,
            vec![
                dm::Label {
                    name: "env".to_owned(),
                    value: "parent".to_owned(),
                },
                dm::Label {
                    name: "env".to_owned(),
                    value: "child".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn descriptor_core_openmetrics_label_encoder_error_is_propagated() {
        let mut registry = Registry::default();
        registry.register(
            "flaky",
            "Flaky metric",
            super::FlakyMetric {
                labels: super::FlakyLabels {
                    ok: "fine".to_owned(),
                },
            },
        );

        assert!(encode_openmetrics(&registry).is_err());
    }

    #[test]
    fn descriptor_core_openmetrics_native_only_histogram_errors() {
        let mut registry = Registry::default();
        let histogram = Histogram::new_native(NativeHistogramConfig::with_schema(0));
        registry.register("native_histogram", "Native histogram", histogram.clone());
        histogram.observe(1.0);

        assert!(encode_openmetrics(&registry).is_err());
    }

    #[test]
    fn descriptor_core_openmetrics_visits_stable_descriptor_order() {
        let registry = build_ordered_registry();

        let metric_set = encode_openmetrics(&registry).unwrap();
        let names: Vec<&str> = metric_set
            .metric_families
            .iter()
            .map(|family| family.name.as_str())
            .collect();

        // OpenMetrics family names drop the unit, normalize to compare the
        // shared order.
        assert_eq!(names, vec!["alpha", "delta", "sub_beta", "sub_gamma"],);
        assert_eq!(
            names
                .iter()
                .map(|name| name.replace("sub_beta", "sub_beta_bytes"))
                .collect::<Vec<_>>(),
            EXPECTED_DESCRIPTOR_ORDER
        );
    }
}

/// Writer-independent sink that records the final base name of every visited
/// descriptor without producing any exposition format.
#[cfg(feature = "test-util")]
#[derive(Default)]
struct RecordingSink {
    descriptors: Vec<String>,
}

#[cfg(feature = "test-util")]
impl prometheus_client::descriptor::DescriptorSink for RecordingSink {
    fn emit_metric(
        &mut self,
        descriptor: prometheus_client::descriptor::DescriptorView<'_>,
        prefixes: &[&str],
        _const_labels: &[&[prometheus_client::descriptor::ConstLabel]],
        _metric: &dyn EncodeMetric,
    ) -> Result<(), fmt::Error> {
        self.descriptors
            .push(prometheus_client::descriptor::descriptor_base_name(
                descriptor, prefixes, true,
            ));
        Ok(())
    }

    fn emit_collector(
        &mut self,
        _prefixes: &[&str],
        _const_labels: &[&[prometheus_client::descriptor::ConstLabel]],
        _collector: &dyn prometheus_client::collector::Collector,
    ) -> Result<(), fmt::Error> {
        Ok(())
    }
}

/// Shared probe: the third, writer-independent recording sink sees exactly the
/// same descriptor order the text and protobuf adapters observe.
#[cfg(feature = "test-util")]
#[test]
fn descriptor_core_shared_order_probe_matches_both_formats() {
    let registry = build_ordered_registry();

    let mut recording = RecordingSink::default();
    registry.visit_with(&mut recording).unwrap();
    assert_eq!(
        recording
            .descriptors
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        EXPECTED_DESCRIPTOR_ORDER
    );

    println!("stable descriptor order:");
    for descriptor in &recording.descriptors {
        println!("{descriptor}");
    }

    let mut text = String::new();
    encode_text(&mut text, &registry).unwrap();
    let text_order: Vec<&str> = text
        .lines()
        .filter_map(|line| line.strip_prefix("# HELP "))
        .map(|line| line.split(' ').next().unwrap())
        .collect();
    assert_eq!(text_order, recording.descriptors);

    #[cfg(feature = "prometheus_protobuf")]
    {
        let families = prometheus_client::encoding::prometheus_protobuf::encode(&registry).unwrap();
        let proto_order: Vec<&str> = families
            .iter()
            .map(|family| {
                family
                    .name
                    .strip_suffix("_total")
                    .unwrap_or(family.name.as_str())
            })
            .collect();
        assert_eq!(proto_order, text_order);
    }
}
