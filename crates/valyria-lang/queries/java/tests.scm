; JUnit 4 and 5 both annotate; there is no naming convention to fall back
; on, so the annotation is the only signal.

(
  (method_declaration
    (modifiers (marker_annotation name: (identifier) @_ann))
    name: (identifier) @test.name) @test
  (#match? @_ann "^(Test|ParameterizedTest|RepeatedTest)$")
)

(
  (method_declaration
    (modifiers (annotation name: (identifier) @_ann))
    name: (identifier) @test.name) @test
  (#match? @_ann "^(Test|ParameterizedTest|RepeatedTest)$")
)
