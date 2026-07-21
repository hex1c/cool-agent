<!-- markdownlint-disable MD013 -->

# Rust 1.97 Lambda/SAM Build Spike

**Status:** Complete locally; repeat in CI and staging before deployment
**Task:** Phase 0, Task 3 / Phase 1 Task 9 prerequisite

## Decision

Use Rust 1.97 with `cargo-lambda` behind the AWS SAM `makefile` build method.
The Lambda runtime is `provided.al2023`, the handler is the conventionally
named `bootstrap` executable, and the initial architecture is `x86_64`.
The make target is kept beside the spike crate so SAM invokes the same command
that developers can run locally.

## Reproduction

From the repository root:

```bash
rustup run 1.97.0 rustc --version
sam validate --lint --template-file infrastructure/spikes/rust-lambda.yaml
sam build --template-file infrastructure/spikes/rust-lambda.yaml
sam local invoke RustBuildSpike --event tests/fixtures/lambda/ping.json
```

The local invoke command requires Docker. The build artifact must contain a
`bootstrap` executable at the ZIP root. No credentials or customer data are
needed for this spike.

## Evidence

| Check                   | Result                                                                               |
| ----------------------- | ------------------------------------------------------------------------------------ |
| Rust version            | 1.97.0                                                                               |
| SAM template validation | Passed: `sam validate --lint --template-file infrastructure/spikes/rust-lambda.yaml` |
| SAM build               | Passed: `sam build --template-file infrastructure/spikes/rust-lambda.yaml`           |
| Artifact layout         | Passed: `bootstrap` at the artifact root                                             |
| Local invoke            | Passed with `sam local invoke` and `tests/fixtures/lambda/ping.json`                 |

The repository records the repeatable path; command output should be appended
by the operator after a successful local/CI run rather than committing build
artifacts.
