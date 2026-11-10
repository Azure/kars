import rust
import codeql.rust.dataflow.DataFlow
import codeql.rust.dataflow.TaintTracking
import codeql.rust.dataflow.internal.Node as Nodes
import codeql.rust.security.UnsafeDeserializationExtensions

module Config implements DataFlow::ConfigSig {
  predicate isSource(DataFlow::Node node) { node instanceof DataFlow::ParameterNode }
  predicate isSink(DataFlow::Node node) { node instanceof UnsafeDeserialization::Sink }
  predicate isBarrier(DataFlow::Node node) { node instanceof UnsafeDeserialization::Barrier }
}
module Flow = TaintTracking::Global<Config>;

from Call call, string name, boolean sinkRetained, boolean untrustedFlow
where
  name = call.getEnclosingCallable().(Function).getName().getText() and
  name = ["case_bounded_str", "case_bounded_bytes", "case_bounded_alias",
          "case_bounded_annotated", "case_unbounded", "case_custom_deserialize", "case_generic",
          "case_mixed_sinks"] and
  call.getStaticTarget().getName().getText() = ["from_str", "from_slice"] and
  (if exists(DataFlow::Node sink |
    sink.(Nodes::FlowSummaryNode).getSummaryNode().getSourceSinkReportingElement() =
      call.getPositionalArgument(0) and Config::isSink(sink))
   then sinkRetained = true else sinkRetained = false) and
  (if exists(DataFlow::Node source, DataFlow::Node sink |
    sink.(Nodes::FlowSummaryNode).getSummaryNode().getSourceSinkReportingElement() =
      call.getPositionalArgument(0) and Flow::flow(source, sink))
   then untrustedFlow = true else untrustedFlow = false)
select name, sinkRetained, untrustedFlow
