#!/usr/bin/env python3
"""Deterministic public-contract fake used by Code System Graph adapter tests."""

import json
import pathlib
import sys
import time


def emit(value):
    print(json.dumps(value, separators=(",", ":")), flush=True)


def mcp_server(mode):
    if mode == "slow-mcp":
        time.sleep(2)
    if mode == "invalid-mcp":
        print("not-json", flush=True)
        return
    for line in sys.stdin:
        message = json.loads(line)
        method = message.get("method")
        if method == "initialize":
            emit(
                {
                    "jsonrpc": "2.0",
                    "id": message["id"],
                    "result": {
                        "protocolVersion": "2024-11-05",
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "codegraph", "version": "1.5.0"},
                    },
                }
            )
        elif method == "tools/list":
            emit(
                {
                    "jsonrpc": "2.0",
                    "id": message["id"],
                    "result": {
                        "tools": [
                            {
                                "name": "codegraph_explore",
                                "description": "Explore source and local context.",
                                "inputSchema": {
                                    "type": "object",
                                    "properties": {
                                        "query": {"type": "string"},
                                        "maxFiles": {"type": "number"},
                                        "projectPath": {"type": "string"},
                                    },
                                    "required": ["query"],
                                },
                            }
                        ]
                    },
                }
            )
        elif method == "tools/call":
            emit(
                {
                    "jsonrpc": "2.0",
                    "id": message["id"],
                    "result": {
                        "content": [
                            {"type": "text", "text": "ephemeral local context"}
                        ],
                        "isError": False,
                    },
                }
            )


def main():
    mode = pathlib.Path(sys.argv[0]).name.removeprefix("codegraph-")
    command = sys.argv[1] if len(sys.argv) > 1 else ""
    if command == "--version":
        print("1.5.0")
    elif command == "serve":
        mcp_server(mode)
    elif command == "status":
        emit(
            {
                "initialized": True,
                "version": "1.5.0",
                "pendingChanges": {"added": 0, "modified": 0, "removed": 0},
                "worktreeMismatch": None,
                "index": {"reindexRecommended": False, "state": "complete"},
            }
        )
    elif command == "query":
        if mode == "large":
            print("x" * 10000)
        elif mode == "slow-cli":
            time.sleep(1)
        else:
            emit(
                [
                    {
                        "node": {
                            "id": "function:fixture",
                            "name": "anchor",
                            "qualifiedName": "fixture::anchor",
                            "kind": "function",
                            "filePath": "src/lib.rs",
                            "startLine": 7,
                        },
                        "score": 10.0,
                    }
                ]
            )
    elif command in ("callers", "callees"):
        emit(
            {
                "symbol": "anchor",
                command: [
                    {
                        "name": "neighbor",
                        "kind": "function",
                        "filePath": "src/neighbor.rs",
                        "startLine": 9,
                    }
                ],
            }
        )
    elif command == "impact":
        emit(
            {
                "symbol": "anchor",
                "depth": 2,
                "nodeCount": 1,
                "affected": [
                    {
                        "name": "neighbor",
                        "kind": "function",
                        "filePath": "src/neighbor.rs",
                        "startLine": 9,
                    }
                ],
            }
        )
    elif command == "affected":
        emit(
            {
                "changedFiles": ["src/lib.rs"],
                "affectedTests": ["tests/anchor.rs"],
                "totalDependentsTraversed": 3,
            }
        )
    elif command == "explore":
        print("CLI local context")
    else:
        print(f"unsupported fake command: {command}", file=sys.stderr)
        raise SystemExit(2)


if __name__ == "__main__":
    main()
