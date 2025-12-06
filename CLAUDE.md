# Waddle Development Guide

## Build System

### cuenv
cuenv is the task runner for this project. Tasks are defined in `env.cue` files using CUE configuration language.

**Always run `cuenv ci` before committing code** to ensure lint, type-check, and tests pass.

Common commands:
- `cuenv task install` - Install dependencies
- `cuenv task lint` - Run linter
- `cuenv task tsc` - Run TypeScript type-check
- `cuenv task test` - Run tests
- `cuenv t install lint tsc test` - Run multiple tasks (shorthand)

Task definitions are in:
- `cuenv/data-service.cue` - Shared tasks for data services
- `*/env.cue` - Service-specific configuration

### projen
projen generates boilerplate files for services. Generated files are read-only.

To regenerate files for a service:
```bash
cd waddle/services/<service-name>
bun run .projenrc.ts
```

Configuration is in `.projenrc.ts` files. The generator is in `generators/projen-data-service/`.
