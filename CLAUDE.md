# Claude Code Guidelines

## Runtime

This project uses **Bun** as its JavaScript/TypeScript runtime. Prefer Bun APIs over Node.js equivalents:

- Use `Bun.file()` and `Bun.write()` instead of `fs.readFileSync`/`fs.writeFileSync`
- Use `await Bun.file(path).exists()` instead of `fs.existsSync()`
- Use `bun test` for running tests
- Use `bun run` for scripts

## Code Organization

**Group by function, not by type.**

### Bad (grouped by type)
```
src/
├── controllers/
│   ├── user.ts
│   └── order.ts
├── services/
│   ├── user.ts
│   └── order.ts
├── types/
│   ├── user.ts
│   └── order.ts
└── validators/
    ├── user.ts
    └── order.ts
```

### Good (grouped by function)
```
src/
├── user/
│   └── index.ts      # controller, service, types, validators
├── order/
│   └── index.ts      # controller, service, types, validators
└── shared/
    └── index.ts      # truly shared utilities
```

When working on "user" functionality, everything is in one place. Related code changes together.

## Project Structure

- `crates/` - Rust crates for core functionality
- `docs/` - Project documentation
- `scripts/` - Development tooling
