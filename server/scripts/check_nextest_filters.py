#!/usr/bin/env python3
"""Fail when a nextest.toml override filter names a binary or test path that no longer exists.

Usage: check_nextest_filters.py NEXTEST_TOML FULL_INVENTORY

FULL_INVENTORY is an unfiltered `cargo nextest list --message-format json`
inventory. Every `binary(...)`, `binary_id(...)`, `package(...)` and `test(...)`
atom in each `[[profile.*.overrides]]` filter must match at least one listed
binary or test. Renaming a module or test binary does not fail compilation, so
without this check an override (for example the serial `postgres` test group)
silently stops applying.
"""

import argparse
import fnmatch
import json
from pathlib import Path
import re
import tomllib

ATOM = re.compile(r"\b(binary_id|binary|package|test)\(([^()]*)\)")
# Default matchers per nextest's filterset DSL.
DEFAULT_MATCHER = {"binary_id": "glob", "binary": "glob", "package": "glob", "test": "contains"}


def matcher(kind, raw):
    """Return a predicate for one atom's name matcher."""
    raw = raw.strip()
    if raw.startswith("="):
        value = raw[1:]
        return lambda name: name == value
    if raw.startswith("~"):
        value = raw[1:]
        return lambda name: value in name
    if raw.startswith("#"):
        value = raw[1:]
        return lambda name: fnmatch.fnmatchcase(name, value)
    if len(raw) >= 2 and raw.startswith("/") and raw.endswith("/"):
        pattern = re.compile(raw[1:-1])
        return lambda name: pattern.search(name) is not None
    if DEFAULT_MATCHER[kind] == "contains":
        return lambda name: raw in name
    return lambda name: fnmatch.fnmatchcase(name, raw)


def filters(config_path):
    with open(config_path, "rb") as stream:
        document = tomllib.load(stream)
    for profile_name, profile in document.get("profile", {}).items():
        for index, override in enumerate(profile.get("overrides", [])):
            yield f"profile.{profile_name}.overrides[{index}]", override["filter"]


def candidates(inventory_path):
    with open(inventory_path, encoding="utf-8") as stream:
        document = json.load(stream)
    names = {"binary_id": set(), "binary": set(), "package": set(), "test": set()}
    for binary_id, suite in document["rust-suites"].items():
        names["binary_id"].add(binary_id)
        names["binary"].add(suite["binary-name"])
        names["package"].add(suite["package-name"])
        names["test"].update(suite["testcases"])
    return names


def dead_atoms(config_path, inventory_path):
    names = candidates(inventory_path)
    dead = []
    for location, expression in filters(config_path):
        for kind, raw in ATOM.findall(expression):
            predicate = matcher(kind, raw)
            if not any(predicate(name) for name in names[kind]):
                dead.append(f"{location}: {kind}({raw.strip()}) matches nothing")
    return dead


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("config", type=Path)
    parser.add_argument("inventory", type=Path)
    args = parser.parse_args()
    dead = dead_atoms(args.config, args.inventory)
    if dead:
        parser.exit(1, "nextest override filters reference missing targets:\n" + "\n".join(dead) + "\n")


if __name__ == "__main__":
    main()
