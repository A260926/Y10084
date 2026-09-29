//! Writer-independent descriptor and label traversal core.
//!
//! Both the text and the protobuf encoders need the same answers for three
//! questions while walking a [`Registry`](crate::registry::Registry):
//!
//! 1. What is the final metric name, given the chain of sub-registry prefixes,
//!    the metric name and its optional unit?
//! 2. In which order are the const labels of the ancestor registries merged
//!    with the metric's own labels?
//! 3. In which order are descriptors visited?
//!
//! None of the three depends on the concrete exposition format. This module
//! answers them once. Format-specific concerns such as the text `# HELP`,
//! `# TYPE`, `# UNIT` and `# EOF` lines or the construction of protobuf
//! messages stay in the respective encoder adapters.

use std::borrow::Cow;
use std::fmt;

use crate::collector::Collector;
use crate::encoding::{EncodeLabel, EncodeMetric, LabelSetEncoder};
use crate::metrics::MetricType;
use crate::registry::Unit;

/// A single const label contributed by a registry.
pub type ConstLabel = (Cow<'static, str>, Cow<'static, str>);

/// Everything the traversal core knows about a single metric descriptor.
///
/// The view deliberately does not carry a pre-computed final name: the name is
/// materialized lazily via [`write_base_name`]/[`base_name`], so that nested
/// sub-registries never allocate a fully concatenated prefix string while
/// walking the tree.
#[derive(Clone, Copy, Debug)]
#[doc(hidden)]
pub struct DescriptorView<'a> {
    /// Metric name as registered, without prefix or unit suffix.
    pub name: &'a str,
    /// Help text including the trailing full stop added by the registry.
    pub help: &'a str,
    /// Optional metric unit.
    pub unit: Option<&'a Unit>,
    /// OpenMetrics metric type inferred from the metric.
    pub metric_type: MetricType,
}

/// Sink receiving the descriptors and samples discovered by the traversal.
///
/// Implemented by the writer-specific encoder adapters as well as by the
/// in-crate order probe used by the shared tests.
#[doc(hidden)]
pub trait DescriptorSink {
    /// Visit one descriptor together with the metric it describes.
    ///
    /// `prefixes` is the chain of sub-registry prefix segments, outermost
    /// first. `const_labels` is the chain of const label slices contributed by
    /// the ancestor registries, likewise outermost first.
    fn emit_metric(
        &mut self,
        descriptor: DescriptorView<'_>,
        prefixes: &[&str],
        const_labels: &[&[ConstLabel]],
        metric: &dyn EncodeMetric,
    ) -> Result<(), fmt::Error>;

    /// Visit a collector, handing the sink the scoped encoder the collector
    /// emits its (possibly multiple) descriptors through.
    fn emit_collector(
        &mut self,
        prefixes: &[&str],
        const_labels: &[&[ConstLabel]],
        collector: &dyn Collector,
    ) -> Result<(), fmt::Error>;
}

/// Final metric name without type-specific suffixes (`_total`, `_info`).
#[doc(hidden)]
pub fn descriptor_base_name(
    descriptor: DescriptorView<'_>,
    prefixes: &[&str],
    include_unit: bool,
) -> String {
    base_name(descriptor, prefixes, include_unit)
}

/// Returns `true` if any registry in the chain contributes at least one const
/// label.
///
/// The text encoder uses it to decide whether braces need to be emitted
/// without iterating (and copying) the complete label set on the hot path.
pub(crate) fn has_const_labels(chains: &[&[ConstLabel]]) -> bool {
    chains.iter().any(|chain| !chain.is_empty())
}

/// Writes the final metric name, i.e. `prefix_name` optionally followed by the
/// `_unit` suffix.
///
/// Prefix segments are joined with underscores without allocating an
/// intermediate concatenated prefix string.
pub(crate) fn write_base_name(
    writer: &mut dyn fmt::Write,
    descriptor: DescriptorView<'_>,
    prefixes: &[&str],
    include_unit: bool,
) -> fmt::Result {
    for prefix in prefixes {
        writer.write_str(prefix)?;
        writer.write_str("_")?;
    }
    writer.write_str(descriptor.name)?;
    if include_unit {
        if let Some(unit) = descriptor.unit {
            writer.write_str("_")?;
            writer.write_str(unit.as_str())?;
        }
    }
    Ok(())
}

/// Owned counterpart of [`write_base_name`], used by adapters that need a
/// `String` (e.g. the protobuf message builders).
pub(crate) fn base_name(
    descriptor: DescriptorView<'_>,
    prefixes: &[&str],
    include_unit: bool,
) -> String {
    let mut name = String::new();
    write_base_name(&mut name, descriptor, prefixes, include_unit)
        .expect("writing to a String not to fail");
    name
}

/// Encodes all const label chains into the given label set in stable order:
/// outermost registry labels first, registration order within each registry.
///
/// Every label flows through the same encoder pipeline as user supplied
/// labels, so an encoder error (e.g. [`fmt::Error`]) is propagated to the
/// caller exactly once, here.
pub(crate) fn encode_const_labels(
    encoder: &mut LabelSetEncoder<'_>,
    chains: &[&[ConstLabel]],
) -> Result<(), fmt::Error> {
    for chain in chains {
        for label in chain.iter() {
            let label_encoder = encoder.encode_label();
            label.encode(label_encoder)?;
        }
    }
    Ok(())
}
