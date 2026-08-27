#!/usr/bin/env python3
"""Print the manifests and source directories in a Cargo local dependency closure."""

import sys
from pathlib import Path


MINIMUM_PYTHON = (3, 11)


def fail(message):
    print("native telemetry Cargo closure: {0}".format(message), file=sys.stderr)
    return 1


if sys.version_info < MINIMUM_PYTHON:
    sys.exit(
        fail(
            "python3 3.11 or newer is required (found {0}.{1}.{2})".format(
                *sys.version_info[:3]
            )
        )
    )

import tomllib


class ClosureError(Exception):
    pass


class CargoClosure:
    def __init__(self):
        self.documents = {}
        self.visited = set()
        self.paths = []

    def load(self, manifest):
        manifest = manifest.resolve()
        if manifest in self.documents:
            return self.documents[manifest]
        try:
            with manifest.open("rb") as source:
                document = tomllib.load(source)
        except (OSError, tomllib.TOMLDecodeError) as error:
            raise ClosureError(
                "failed to parse Cargo manifest {0}: {1}".format(manifest, error)
            ) from error
        self.documents[manifest] = document
        return document

    def dependency_tables(self, document):
        for section in ("dependencies", "build-dependencies"):
            dependencies = document.get(section)
            if isinstance(dependencies, dict):
                yield dependencies

        targets = document.get("target")
        if not isinstance(targets, dict):
            return
        for target in targets.values():
            if not isinstance(target, dict):
                continue
            for section in ("dependencies", "build-dependencies"):
                dependencies = target.get(section)
                if isinstance(dependencies, dict):
                    yield dependencies

    def workspace_manifest(self, crate_dir):
        for directory in (crate_dir, *crate_dir.parents):
            manifest = directory / "Cargo.toml"
            if not manifest.is_file():
                continue
            document = self.load(manifest)
            if isinstance(document.get("workspace"), dict):
                return manifest.resolve(), document
        return None

    def workspace_dependency_path(self, crate_dir, dependency_name):
        workspace = self.workspace_manifest(crate_dir)
        if workspace is None:
            return None
        manifest, document = workspace
        workspace_table = document["workspace"]
        dependencies = workspace_table.get("dependencies")
        if not isinstance(dependencies, dict):
            return None
        specification = dependencies.get(dependency_name)
        if not isinstance(specification, dict):
            return None
        dependency_path = specification.get("path")
        if not isinstance(dependency_path, str):
            return None
        return (manifest.parent / dependency_path).resolve()

    def local_dependency_dirs(self, manifest, document):
        crate_dir = manifest.parent
        for dependencies in self.dependency_tables(document):
            for name, specification in dependencies.items():
                if not isinstance(specification, dict):
                    continue
                dependency_path = specification.get("path")
                if isinstance(dependency_path, str):
                    yield (crate_dir / dependency_path).resolve()
                elif specification.get("workspace") is True:
                    workspace_path = self.workspace_dependency_path(crate_dir, name)
                    if workspace_path is not None:
                        yield workspace_path

    def visit(self, manifest):
        manifest = manifest.resolve()
        if manifest in self.visited:
            return
        document = self.load(manifest)
        self.visited.add(manifest)
        self.paths.extend((manifest, manifest.parent / "src"))
        for dependency_dir in self.local_dependency_dirs(manifest, document):
            self.visit(dependency_dir / "Cargo.toml")

    def discover(self, roots):
        for root in roots:
            self.visit(root)
        return self.paths


def main(arguments):
    if not arguments:
        return fail("expected at least one Cargo.toml path")
    try:
        paths = CargoClosure().discover([Path(argument) for argument in arguments])
    except ClosureError as error:
        return fail(str(error))
    for path in paths:
        print(path)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
