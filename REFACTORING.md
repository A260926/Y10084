# Descriptor & label traversal core refactoring

## Goal

Converge the parallel name-composition, const-label-merging and stable-order
logic that used to live independently in the text encoder and the two protobuf
encoders into one writer-agnostic core, without changing any public output,
trait, method signature or feature behavior.

## Old call chain

Before the refactoring, every encoder re-implemented the same semantics on its
own:

1. `Registry::encode` (src/registry.rs) passed `prefix` and `labels` as two
   separate slices into `DescriptorEncoder::with_prefix_and_labels`
   (src/encoding.rs), which forwarded them to each format's
   `DescriptorEncoder::with_prefix_and_labels`
   (src/encoding/text.rs, src/encoding/prometheus_protobuf.rs,
   src/encoding/openmetrics_protobuf.rs).
2. The text encoder inlined `prefix "_" name "_" unit` string assembly four
   times (HELP line, TYPE line, UNIT line, and
   `MetricEncoder::write_prefix_name_unit` for every sample line).
3. `prometheus_protobuf::metric_family_name` re-assembled the same
   `prefix "_" name "_" unit` name into a `String`, and
   `openmetrics_protobuf::DescriptorEncoder::encode_descriptor` did it a third
   time with a `match` on the prefix.
4. Label-group ordering (const labels, then additional labels, then family
   labels, with presence-based separator decisions) was hard-coded inside
   `text::MetricEncoder::encode_labels`, while each protobuf encoder
   re-encoded `self.labels` into a `Vec<LabelPair>` on its own.

## New boundary

`src/encoding/descriptor_core.rs` is the writer-agnostic core. It contains no
format-specific output and no enum over encoding details:

- `Scope` (src/encoding/descriptor_core.rs:28): borrowed view of a
  (sub-)registry's prefix and const labels. `Copy`, never allocates, so nested
  sub-registries never re-assemble prefix strings during encoding.
- `Scope::descriptor` (src/encoding/descriptor_core.rs:46): combines a scope
  with a metric name and unit into a `Descriptor`.
- `Descriptor` (src/encoding/descriptor_core.rs:59): the final descriptor.
  `write_name` (src/encoding/descriptor_core.rs:103) streams
  `<prefix>_<name>_<unit>` into any `fmt::Write` without allocation;
  `name_string` (src/encoding/descriptor_core.rs:113) and
  `prefix_name_string` (src/encoding/descriptor_core.rs:128) serve the
  protobuf data models that need owned names.
- `ConstLabels` (src/encoding/descriptor_core.rs:148): borrowed const-label
  view in stable registration order; traversing it never copies the label set,
  so the hot path of plain metrics stays allocation free.
- `LabelOrder` (src/encoding/descriptor_core.rs:166): the shared stable-order
  determination for the const / additional / family label groups, including
  the presence-based separator rules.

Call chain after the refactoring:

1. `Registry::encode` builds one `Scope` per registry
   (src/registry.rs:290) and hands it to `DescriptorEncoder::with_scope`
   (src/encoding.rs:158), which dispatches to each format adapter's
   `with_scope` (src/encoding/text.rs:206,
   src/encoding/prometheus_protobuf.rs:158,
   src/encoding/openmetrics_protobuf.rs:84).
2. Each adapter's `encode_descriptor` resolves the final `Descriptor` via
   `Scope::descriptor` and only then applies format-specific output:
   - text: HELP/TYPE/UNIT lines and `# EOF` stay in src/encoding/text.rs
     (`encode_descriptor` at src/encoding/text.rs:213, `encode_eof` at
     src/encoding/text.rs:180); sample lines go through
     `write_prefix_name_unit` (src/encoding/text.rs:459) which now delegates
     to `Descriptor::write_name`; label merging uses `LabelOrder`
     (src/encoding/text.rs:477).
   - prometheus protobuf: `MetricFamily` message construction stays in
     src/encoding/prometheus_protobuf.rs (`encode_descriptor` at
     src/encoding/prometheus_protobuf.rs:165), only the base name comes from
     `Descriptor::name_string` (src/encoding/prometheus_protobuf.rs:130).
   - openmetrics protobuf: same split at
     src/encoding/openmetrics_protobuf.rs:91.

## Format differences deliberately not merged

1. Unit placement in the metric family name. The text format and the
   Prometheus protobuf format append `_<unit>` to the exposed metric name
   (`Descriptor::name_string`, used at
   src/encoding/prometheus_protobuf.rs:130), while the OpenMetrics protobuf
   format keeps the unit out of the family name and carries it in the
   dedicated `MetricFamily.unit` field instead
   (`Descriptor::prefix_name_string` at
   src/encoding/openmetrics_protobuf.rs:100). Merging these would change the
   wire output of one of the formats.
2. Metric-type suffix placement. The Prometheus protobuf format folds the
   `_total` / `_info` suffixes into the family name itself
   (`metric_family_name` at src/encoding/prometheus_protobuf.rs:129), whereas
   the text format leaves the family name bare and appends the suffix per
   sample line (`write_suffix`, called from `encode_counter` /
   `encode_info` in src/encoding/text.rs). The core therefore exposes only the
   suffix-free base name; each adapter keeps its own suffix policy.

## Behavior guards

`src/encoding/descriptor_core.rs` (`mod tests`) locks the pre-refactoring
behavior with exact-byte assertions for both the text and the Prometheus
protobuf format: prefix+unit composition, parent/child same-name labels,
empty label sets, help text containing backslashes and newlines, `fmt::Error`
propagation from a failing label encoder, native histograms, and empty
families. `descriptor_core_shared_probe_order` prints and asserts the stable
descriptor order observed identically by both encoders.
