; `#[test]`, `#[tokio::test]`, `#[test_case]`, and friends: any attribute
; naming `test` that sits directly above a function.
;
; `#[cfg(test)]` also contains the word, but it precedes a `mod`, not a
; `function_item`, so the anchored sibling pattern below does not fire on
; it. The one case it does catch — `#[cfg(test)] fn helper()` — is a
; test-only function, which is the right answer anyway.

(
  (attribute_item) @_attr
  .
  (function_item name: (identifier) @test.name) @test
  (#match? @_attr "\\btest\\b")
)
