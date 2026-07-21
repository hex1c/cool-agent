# Document test fixtures

All byte fixtures for image, PDF, CSV, and Office normalization tests are constructed inline in
the test source files (`crates/application/tests/image_pdf_security.rs`,
`crates/application/tests/office_security.rs`, and unit tests within the
normalization modules). No external binary fixtures are committed to this
directory. `office-cases.json` is a human-readable catalogue of the inline
Office/CSV cases, not a binary fixture.

## Why inline fixtures?

- **Auditability:** Every byte in a test fixture is visible in the test source;
  there is no risk of committing a malicious binary blob disguised as a
  "fixture."
- **Portability:** No need to ship binary files alongside the source tree.
- **Reproducibility:** Exact bytes are encoded in the test, eliminating
  cross-platform line-ending or encoding issues.

## Adding new fixtures

Build helpers that produce minimal valid structures (JPEG, PNG, PDF) and
malformed variants (truncated headers, bombs, encrypted). Keep helpers in the
test file; do not drop binary files here unless explicitly approved.

## Task mapping

| Task | Test file | What it covers |
| ------ | ----------- | --------------- |
| 27 | `crates/application/tests/image_pdf_security.rs` | JPEG/PNG sniffing, dimension parsing, pixel limits, PDF page counting, encryption rejection, decompression bomb detection |
| 28 | `crates/application/tests/office_security.rs` | CSV dialect/encoding/row/cell limits, OOXML ZIP-header macro/encrypted/bomb screening, legacy `.doc`/`.xls` rejection, macro-enabled MIME rejection |
