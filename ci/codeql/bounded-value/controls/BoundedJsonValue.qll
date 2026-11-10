/** Experimental sink-local size proof. Not enabled in the production suite. */
import rust
private import codeql.rust.dataflow.DataFlow
private import codeql.rust.dataflow.internal.Node as Nodes
private import codeql.rust.internal.typeinference.TypeInference as TI
private import codeql.rust.internal.typeinference.Type as Types
private import codeql.rust.frameworks.stdlib.Builtins as Builtins

int smallConstant(Expr expression) {
  result = expression.(LiteralExpr).getTextValue().replaceAll("_", "").toInt() and
  result >= 0 and result <= 4194304
  or
  result = smallConstant(expression.(ConstAccess).getConst().getBody())
  or
  exists(BinaryExpr product, int left, int right |
    expression = product and product.getOperatorName() = "*" and
    left = smallConstant(product.getLhs()) and right = smallConstant(product.getRhs()) and
    result = left * right and result <= 4194304
  )
}

predicate immutableBytesOrStr(Expr expression) {
  exists(Types::RefSharedType reference, TI::TypePath innerPath, Types::Type inner |
    reference = TI::inferTypeCertain(expression, TI::TypePath::nil()) and
    innerPath = TI::TypePath::singleton(reference.getPositionalTypeParameter(0)) and
    inner = TI::inferTypeCertain(expression, innerPath) and
    (
      inner.(Types::StructType).getTypeItem() instanceof Builtins::Str
      or
      exists(Types::SliceType slice, TI::TypePath bytePath |
        inner = slice and
        bytePath = TI::TypePath::snoc(innerPath, slice.getPositionalTypeParameter(0)) and
        TI::inferTypeCertain(expression, bytePath).(Types::StructType).getTypeItem()
          instanceof Builtins::U8
      )
    )
  )
}

predicate lengthRejection(AstNode guard, Expr input, boolean branch) {
  exists(BinaryExpr comparison, MethodCallExpr length, Crate standard, int maximum |
    guard = comparison and comparison.getLhs() = length and
    comparison.getOperatorName() = ">" and
    maximum = smallConstant(comparison.getRhs()) and maximum > 0 and
    length.getStaticTarget().getName().getText() = "len" and
    standard.getName() = ["core", "alloc", "std"] and
    exists(length.getStaticTarget().getCanonicalPath(standard)) and
    input = length.getReceiver() and immutableBytesOrStr(input) and branch = false
  )
}

// Match shared inference's unique non-pseudo semantics; propagation markers are
// not concrete alternatives. Multiple concrete types still fail the proof.
private Types::Type uniqueInferredType(AstNode node, TI::TypePath path) {
  result = unique(Types::Type candidate |
    candidate = TI::inferType(node, path) and not candidate instanceof Types::PseudoType
  | candidate)
}

predicate boundedStandardValueSink(DataFlow::Node node) {
  exists(Call call, Crate serde, Types::DataType returnType, TI::TypePath successPath,
         Types::EnumType value |
    node.asExpr() = call.getPositionalArgument(0) and
    node = DataFlow::BarrierGuard<lengthRejection/3>::getABarrierNode() and
    serde.getName() = "serde_json" and
    call.getStaticTarget().getCanonicalPath(serde) =
      ["serde_json::de::from_slice", "serde_json::de::from_str"] and
    returnType = uniqueInferredType(call, TI::TypePath::nil()) and
    successPath = TI::TypePath::singleton(returnType.getPositionalTypeParameter(0)) and
    value = uniqueInferredType(call, successPath) and
    value.getTypeItem().getCanonicalPath(serde) = "serde_json::value::Value"
  )
}

predicate boundedStandardValueModeledSink(DataFlow::Node sink) {
  exists(DataFlow::Node argument |
    sink.(Nodes::FlowSummaryNode).getSummaryNode().getSourceSinkReportingElement() =
      argument.asExpr() and
    boundedStandardValueSink(argument)
  )
}
