; Jest, Vitest, Mocha, and Jasmine all share the `it("name", fn)` /
; `test("name", fn)` shape. The test's name is a string literal rather than
; an identifier, which is why `@test.name` here points at a
; `string_fragment`.

(
  (call_expression
    function: (identifier) @_fn
    arguments: (arguments . (string (string_fragment) @test.name))) @test
  (#match? @_fn "^(it|test)$")
)
