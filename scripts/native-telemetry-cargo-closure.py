#!/usr/bin/env python3
"""Print the manifests and crate directories in a Cargo local dependency closure.

Repository and workspace Cargo configuration is included. CARGO_HOME
configuration is intentionally outside this checker's scope.
"""

import os
import sys
from pathlib import Path
from urllib.parse import unquote


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
        self.visited_configs = set()
        self.visited_override_manifests = set()
        self.paths = []
        self.repository_root = Path(
            os.environ.get("WADDLE_NATIVE_TELEMETRY_ROOT", Path.cwd())
        ).resolve()

    def load(self, path, description="Cargo manifest"):
        path = path.resolve()
        if path in self.documents:
            return self.documents[path]
        try:
            with path.open("rb") as source:
                document = tomllib.load(source)
        except (OSError, tomllib.TOMLDecodeError) as error:
            raise ClosureError(
                "failed to parse {0} {1}: {2}".format(description, path, error)
            ) from error
        self.documents[path] = document
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
        return self.local_specification_dir(manifest, specification)

    def local_git_dir(self, declaring_file, git_source, relative_root=None):
        if not git_source.startswith("file:"):
            return None
        target = git_source[5:]
        if target.startswith("//"):
            target = target[2:]
        target = unquote(target)
        if not target:
            raise ClosureError(
                "local git file source in {0} has an empty target".format(
                    declaring_file
                )
            )
        dependency_dir = Path(target)
        if not dependency_dir.is_absolute():
            dependency_dir = (relative_root or declaring_file.parent) / dependency_dir
        dependency_dir = dependency_dir.resolve()
        if not dependency_dir.is_dir():
            raise ClosureError(
                "local git file source in {0} targets missing directory {1}".format(
                    declaring_file, dependency_dir
                )
            )
        manifest = dependency_dir / "Cargo.toml"
        if not manifest.is_file():
            repository_kind = "bare git repository" if (
                (dependency_dir / "HEAD").is_file()
                and (dependency_dir / "objects").is_dir()
            ) else "directory without Cargo.toml"
            raise ClosureError(
                "local git file source in {0} targets {1} ({2})".format(
                    declaring_file, dependency_dir, repository_kind
                )
            )
        return dependency_dir

    def local_specification_dir(
        self, declaring_file, specification, relative_root=None
    ):
        if relative_root is None:
            relative_root = declaring_file.parent
        dependency_path = specification.get("path")
        if isinstance(dependency_path, str):
            return (relative_root / dependency_path).resolve()
        git_source = specification.get("git")
        if isinstance(git_source, str):
            return self.local_git_dir(declaring_file, git_source, relative_root)
        return None

    def local_dependency_dirs(self, manifest, document):
        crate_dir = manifest.parent
        for dependencies in self.dependency_tables(document):
            for name, specification in dependencies.items():
                if not isinstance(specification, dict):
                    continue
                dependency_dir = self.local_specification_dir(
                    manifest, specification
                )
                if dependency_dir is not None:
                    yield dependency_dir
                elif specification.get("workspace") is True:
                    workspace_path = self.workspace_dependency_path(crate_dir, name)
                    if workspace_path is not None:
                        yield workspace_path

    def override_dependency_dirs(self, declaring_file, document, relative_root=None):
        if relative_root is None:
            relative_root = declaring_file.parent
        patch = document.get("patch")
        if isinstance(patch, dict):
            for source in patch.values():
                if not isinstance(source, dict):
                    continue
                for specification in source.values():
                    if not isinstance(specification, dict):
                        continue
                    dependency_dir = self.local_specification_dir(
                        declaring_file, specification, relative_root
                    )
                    if dependency_dir is not None:
                        yield dependency_dir

        replacements = document.get("replace")
        if isinstance(replacements, dict):
            for specification in replacements.values():
                if not isinstance(specification, dict):
                    continue
                dependency_dir = self.local_specification_dir(
                    declaring_file, specification, relative_root
                )
                if dependency_dir is not None:
                    yield dependency_dir

    def root_override_manifest(self, manifest):
        workspace = self.workspace_manifest(manifest.parent)
        if workspace is not None:
            return workspace
        return manifest, self.load(manifest)

    def visit_root_overrides(self, manifest):
        override_manifest, document = self.root_override_manifest(manifest)
        if override_manifest in self.visited_override_manifests:
            return
        self.visited_override_manifests.add(override_manifest)
        for dependency_dir in self.override_dependency_dirs(
            override_manifest, document
        ):
            self.visit(dependency_dir / "Cargo.toml")

    def cargo_config_paths(self, crate_dir):
        directory = crate_dir.resolve()
        while True:
            cargo_dir = directory / ".cargo"
            config = cargo_dir / "config"
            config_toml = cargo_dir / "config.toml"
            if config.is_file():
                yield config.resolve()
            elif config_toml.is_file():
                yield config_toml.resolve()
            if directory == self.repository_root or directory.parent == directory:
                return
            directory = directory.parent

    def config_override_dependency_dirs(self, config, document):
        relative_root = config.parent.parent
        paths = document.get("paths")
        if isinstance(paths, list):
            for dependency_path in paths:
                if isinstance(dependency_path, str):
                    yield (relative_root / dependency_path).resolve()
        yield from self.override_dependency_dirs(config, document, relative_root)

    def config_local_sources(self, config, document):
        sources = document.get("source")
        if not isinstance(sources, dict):
            return
        relative_root = config.parent.parent
        emitted = set()
        for source_name in sources:
            current_name = source_name
            chain = []
            while True:
                if current_name in chain:
                    raise ClosureError(
                        "Cargo config {0} has a source replace-with cycle: {1}".format(
                            config, " -> ".join((*chain, current_name))
                        )
                    )
                chain.append(current_name)
                specification = sources.get(current_name)
                if not isinstance(specification, dict):
                    break

                local_kind = None
                local_path = None
                for kind in ("directory", "local-registry"):
                    value = specification.get(kind)
                    if isinstance(value, str):
                        local_kind = kind
                        local_path = Path(value)
                        if not local_path.is_absolute():
                            local_path = relative_root / local_path
                        local_path = local_path.resolve()
                        break
                if local_path is None:
                    git_source = specification.get("git")
                    if isinstance(git_source, str):
                        local_path = self.local_git_dir(
                            config, git_source, relative_root
                        )
                        if local_path is not None:
                            local_kind = "git"

                if local_path is not None:
                    key = (local_kind, local_path)
                    if key not in emitted:
                        emitted.add(key)
                        yield local_kind, local_path
                    break

                replacement = specification.get("replace-with")
                if not isinstance(replacement, str):
                    break
                current_name = replacement

    def visit_config_local_source(self, config, kind, source_dir):
        if not source_dir.is_dir():
            raise ClosureError(
                "Cargo config {0} local {1} source targets missing directory {2}".format(
                    config, kind, source_dir
                )
            )
        if source_dir not in self.paths:
            self.paths.append(source_dir)
        if kind == "git":
            self.visit(source_dir / "Cargo.toml")
        elif kind == "directory":
            for child in sorted(source_dir.iterdir()):
                if child.is_dir() and (child / "Cargo.toml").is_file():
                    self.visit(child / "Cargo.toml")

    def visit_cargo_configs(self, crate_dir):
        for config in self.cargo_config_paths(crate_dir):
            if config in self.visited_configs:
                continue
            self.visited_configs.add(config)
            document = self.load(config, "Cargo config")
            for dependency_dir in self.config_override_dependency_dirs(
                config, document
            ):
                self.visit(dependency_dir / "Cargo.toml")
            for kind, source_dir in self.config_local_sources(config, document):
                self.visit_config_local_source(config, kind, source_dir)

    def visit(self, manifest):
        manifest = manifest.resolve()
        if manifest in self.visited:
            return
        document = self.load(manifest)
        self.visited.add(manifest)
        self.paths.extend((manifest, manifest.parent))
        self.visit_root_overrides(manifest)
        self.visit_cargo_configs(manifest.parent)
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
