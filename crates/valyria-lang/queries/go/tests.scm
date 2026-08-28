; `go test` collects by name prefix, so the naming convention *is* the
; definition of a test here — including benchmarks, fuzz targets, and
; runnable examples, all of which the verification engine can invoke.

(
  (function_declaration name: (identifier) @test.name) @test
  (#match? @test.name "^(Test|Benchmark|Fuzz|Example)")
)
