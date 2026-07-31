# Third-party notices

Code System Graph depends on third-party Rust crates distributed under permissive licenses compatible
with Apache-2.0. `Cargo.lock` records the exact dependency graph.

The release process must generate the authoritative dependency attribution report and SBOM
from the release lockfile. This source-tree notice is not a substitute for that generated
release artifact.

## CodeGraph interoperability

[CodeGraph](https://github.com/colbymchenry/codegraph) is an independent project and is not bundled
or redistributed with Code System Graph. The optional integration communicates with a separately
installed CodeGraph process through its public MCP and CLI interfaces.

CodeGraph is distributed under the MIT License:

```text
Copyright (c) 2026 Colby Mchenry
```

The complete license is available in the
[CodeGraph repository](https://github.com/colbymchenry/codegraph/blob/main/LICENSE).
