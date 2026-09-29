# Descriptor / Label Traversal Refactor

## Goal

The registry combined prefix, metric name, unit and const labels, and both the
text and protobuf encoders independently reproduced that combination:

* the final name was assembled from prefix + name + unit in three places;
* const labels were merged and ordered in the registry, then re-encoded by
  every adapter;
* label encoder errors (`std::fmt::Error`) had to be propagated in every
  adapter separately.

The refactor extracts one writer-independent core for descriptor and label
traversal while leaving every format-specific concern in its adapter. No public
output changes: all pre-existing tests pass byte-for-byte.

## Old call chain

1. An encoder entry point built its writer-specific `DescriptorEncoder`
   (`src/encoding/text.rs`, `src/encoding/prometheus_protobuf.rs`,
   `src/encoding/openmetrics_protobuf.rs`) and called `Registry::encode`.
2. `Registry::encode` (old `src/registry.rs`) iterated metrics, collectors and
   sub-registries. For every non-empty metric it created a scoped encoder with
   `DescriptorEncoder::with_prefix_and_labels(prefix, labels)`.
3. `Registry::sub_registry_with_prefix` eagerly concatenated the whole parent
   prefix (`parent + "_" + child`) and `sub_registry_with_labels` cloned the
   complete parent label `Vec` into the child on every nesting level.
4. Each adapter re-derived the final name itself:
   * text wrote `prefix_name[_unit]` separately for `# HELP`, `# TYPE`,
     `# UNIT` and every sample line;
   * Prometheus protobuf had a dedicated `metric_family_name(...)` and added
     `_total`/`_info` there;
   * OpenMetrics protobuf concatenated `prefix + "_" + name` itself.
5. Each adapter encoded the registry const labels into its own label
   container before samples were produced.

Parallel logic therefore lived in `src/registry.rs`,
`src/encoding/text.rs`, `src/encoding/prometheus_protobuf.rs` and
`src/encoding/openmetrics_protobuf.rs`.

## New boundaries

### Writer-independent core: `src/descriptor.rs`

* `DescriptorView` (`src/descriptor.rs:36`): a copyable view carrying the
  registered `name`, `help`, optional `unit` and `metric_type`. It never owns
  a pre-computed final name.
* `DescriptorSink` (`src/descriptor.rs:52`): the single traversal interface
  with `emit_metric` (`src/descriptor.rs:58`) and `emit_collector`
  (`src/descriptor.rs:68`).
* Shared final-name semantics:
  * `write_base_name` (`src/descriptor.rs:100`) writes
    `prefix_segment_ ..._name[_unit]` directly, joining prefix segments with
    `_` without an intermediate concatenated prefix string;
  * `base_name` / `descriptor_base_name` (`src/descriptor.rs:122`,
    `src/descriptor.rs:78`) return the owned form for message-building
    adapters.
* Shared const-label semantics:
  * `has_const_labels` (`src/descriptor.rs:91`) answers the empty-label-set
    question without iterating or copying the labels;
  * `encode_const_labels` (`src/descriptor.rs:139`) walks the const-label
    chains in stable order (outermost registry first, registration order
    within a registry) through the public `EncodeLabel` pipeline, so an
    encoder error is produced and propagated in exactly one place.

### Traversal owner: `src/registry.rs`

* `Registry` (`src/registry.rs:60`) now stores only its own prefix *segment*
  and its own labels.
* `sub_registry_with_prefix` (`src/registry.rs:241`) and
  `sub_registry_with_labels` (`src/registry.rs:257`) no longer concatenate a
  full prefix string nor clone the merged label set; the child stores its own
  contribution only.
* `Registry::visit` (`src/registry.rs:298`) walks the tree depth-first with
  borrowed prefix (`Vec<&str>`) and label (`Vec<&[ConstLabel]>`) stacks:
  metrics of one level, then that level's collectors, then sub-registries.
  Nested registries therefore allocate neither the full prefix nor the full
  label set. The ordinary metric hot path passes the borrowed slices straight
  through; only the protobuf adapters (which need owned per-metric label
  vectors for the message) allocate the same vectors as before.
* `Registry::encode` (`src/registry.rs:278`) drives any `DescriptorSink`.
  `Registry::visit_with` (`src/registry.rs:288`, gated by `test-util`/in-crate
  tests) lets the shared order probe observe descriptors without a format.

### Format adapters

* The public `encoding::DescriptorEncoder` enum implements `DescriptorSink`
  (`src/encoding.rs:176`), forwarding `emit_metric`/`emit_collector`
  (`src/encoding.rs:177`, `src/encoding.rs:192`) to the active inner adapter.
  Public trait and method signatures (`encode_descriptor`,
  `with_prefix_and_labels` consumers such as `Collector`) behave as before.
* Text (`src/encoding/text.rs`): `DescriptorEncoder` implements the sink at
  `src/encoding/text.rs:249`; the format-specific `# HELP`/`# TYPE`/`# UNIT`
  lines stay in `write_descriptor_lines` (`src/encoding/text.rs:294`), the
  sample name uses the shared `write_base_name`
  (`src/encoding/text.rs:538`), and the `{...}` label block plus its comma
  rules stay format-specific in `encode_labels`
  (`src/encoding/text.rs:557`). `encode_eof` remains the text-only
  `# EOF` marker (`src/encoding/text.rs:184`).
* Prometheus protobuf (`src/encoding/prometheus_protobuf.rs`): sink at
  `src/encoding/prometheus_protobuf.rs:181`; the protobuf `MetricFamily` /
  `Metric` message construction stays in the adapter
  (`encode_visited`, `src/encoding/prometheus_protobuf.rs:206`) and the
  length-delimited wire payload stays in `encode_to_vec`
  (`src/encoding/prometheus_protobuf.rs:69`).
* OpenMetrics protobuf (`src/encoding/openmetrics_protobuf.rs`): sink at
  `src/encoding/openmetrics_protobuf.rs:119`; its `MetricSet` message
  construction stays in `encode_visited`
  (`src/encoding/openmetrics_protobuf.rs:144`).

There is deliberately no mega-enum encoding format details: the core knows
nothing about help lines, EOF, message structs or exemplars.

## Format differences intentionally not merged

1. **Family/sample name suffixes.** The core only produces the shared
   `prefix_name[_unit]` base. Prometheus protobuf appends `_total` for
   counters and `_info` for info metrics when building the family name
   (`src/encoding/prometheus_protobuf.rs:214` and
   `src/encoding/prometheus_protobuf.rs:215`); text appends `_total`/
   `_info`/`sum`/`count`/`bucket` per emitted sample; OpenMetrics protobuf
   keeps the family name free of both unit and `_total`. This is an
   exposition-format difference and stays in each adapter.
2. **OpenMetrics protobuf family name omits the unit.** The Prometheus
   protobuf family name includes the unit (e.g. `sub_requests_seconds_total`)
   while the OpenMetrics protobuf family name is `sub_requests` with the unit
   carried in a separate `unit` field (`src/encoding/openmetrics_protobuf.rs:
   144` vs `src/encoding/prometheus_protobuf.rs:206`). The shared core offers
   the `include_unit` flag rather than forcing one convention.

Two more format-specific behaviors remain by design: the text label block
including brace emission and the comma-prepending family-label writer
(`src/encoding/text.rs:557`) and text-only escaping-free help emission plus
`# EOF` (`src/encoding/text.rs:184`, `src/encoding/text.rs:294`).

## Behavior guards

`tests/descriptor_core.rs` pins the shared semantics and asserts exact bytes
per format: prefix+unit combination, parent/child same-named labels, empty
label set / empty family, help containing `\\` and `\n`, mid-label
`fmt::Error` propagation, native-only histogram error and native histogram
content, plus a shared recording sink proving text and protobuf visit
descriptors in identical order.
