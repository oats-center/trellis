# Verify real exported parentage and durable links without printing span attributes.
def outcome: [.attributes[]? | select(.key == "trellis.outcome") | .value.stringValue] | first;
[.[].resourceSpans[]
 | ([.resource.attributes[] | select(.key == "service.name") | .value.stringValue] | first) as $service
 | .scopeSpans[].spans[]
 | {service: $service, name, traceId, spanId, parentSpanId, links, attributes}]
as $spans
| {
    browserToRust: any($spans[];
      .service == "trellis-server" and .name == "trellis.rpc.server" and
      (. as $child | any($spans[];
        .service == "trellis-browser-console" and .name == "trellis.rpc.client" and
        .traceId == $child.traceId and .spanId == $child.parentSpanId))),
    rustToTypeScript: any($spans[];
      .service == "provider" and .name == "trellis.rpc.server" and
      (. as $child | any($spans[];
        .service == "runtime-rust-caller" and .name == "trellis.rpc.client" and
        .traceId == $child.traceId and .spanId == $child.parentSpanId))),
    durableOperation: any($spans[];
      .service == "runtime-rust-provider" and .name == "trellis.operation.execute.start" and
      (. as $execution | any($execution.links[]?;
        . as $link | any($spans[];
          .service == "runtime-trellis.OperationCaller" and
          .traceId == $link.traceId and .spanId == $link.spanId)))),
    typeScriptOperation: any($spans[];
      .service == "provider" and .name == "trellis.operation.execute.start" and
      (. as $execution | any($execution.links[]?;
        . as $link | any($spans[];
          .name == "trellis.rpc.client" and
          .traceId == $link.traceId and .spanId == $link.spanId)))),
    # One TS Job attempt roots locally and links to the exported producer; the
    # downstream RPC issued after the controlled wait is a child of that
    # attempt's start span in the attempt's own trace, not the producer trace.
    attemptContextChildOfAttempt: any($spans[];
      .service == "provider" and .name == "trellis.job.attempt.start" and
      (. as $attempt | any($spans[];
        .service == "provider" and .name == "trellis.rpc.client" and
        .traceId == $attempt.traceId and .spanId != $attempt.spanId))),
    downstreamRpcChildOfAttempt: any($spans[];
      .service == "provider" and .name == "trellis.rpc.client" and
      (. as $client | any($spans[];
        .service == "provider" and .name == "trellis.job.attempt.start" and
        .traceId == $client.traceId and .spanId == $client.parentSpanId))),
    receiverChildOfExportedClient: any($spans[];
      .service == "provider" and .name == "trellis.rpc.server" and
      (. as $child | any($spans[];
        .service == "provider" and .name == "trellis.rpc.client" and
        .traceId == $child.traceId and .spanId == $child.parentSpanId))),
    # Retry and completion carry distinct attempt contexts but link to the same
    # exported producer.
    durableJob: any($spans[];
      .service == "provider" and .name == "trellis.job.attempt.finish" and outcome == "retry" and
      (. as $retry | any($retry.links[]?;
        . as $link | any($spans[];
          .service == "provider" and .name == "trellis.rpc.server" and
          .traceId == $link.traceId and .spanId == $link.spanId) and
        any($spans[];
          .service == "provider" and .name == "trellis.job.attempt.finish" and
          outcome == "completed" and
          any(.links[]?; .traceId == $link.traceId and .spanId == $link.spanId))))),
    distinctAttemptContexts: (
      [$spans[] | select(.service == "provider" and .name == "trellis.job.attempt.start")
       | .traceId] | unique | length) >= 2,
    noLifetimeAttemptSpan: all($spans[];
      all(select(.service == "provider");
        .name != "trellis.job.attempt")),
    noSecretAttributes: all($spans[];
      all(.attributes[]?; (.key | test("password|proof|secret|token|cookie|credential|payload|body|message|authorization|nats\\.subject"; "i") | not)))
  }
| if all(.[]; .) then . else error("collected trace evidence failed validation") end
