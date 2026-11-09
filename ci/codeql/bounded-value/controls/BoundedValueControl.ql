import rust
import codeql.rust.dataflow.DataFlow
import BoundedJsonValue

from Call call, string name, boolean proven
where
  name = call.getEnclosingCallable().(Function).getName().getText() and
  name.matches("case_%") and
  call.getStaticTarget().getName().getText() = ["from_str", "from_slice"] and
  (if boundedStandardValueSink(any(DataFlow::Node node | node.asExpr() = call.getPositionalArgument(0)))
   then proven = true else proven = false)
select name, proven
