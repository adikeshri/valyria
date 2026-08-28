; pytest and unittest both key off the `test_`/`test` naming convention;
; unittest additionally requires the class to derive from `TestCase`, but
; a `test_`-prefixed method that is not collected is a far cheaper mistake
; than missing every test in the repository.

(
  (function_definition name: (identifier) @test.name) @test
  (#match? @test.name "^test")
)
