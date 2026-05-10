# AGENTS.md

## What this is

dbrain is a persistent memory server for AI clients. Stores identity, conversations, and knowledge (PARA model with hot/warm/cold tiers) in SQLite + FTS5. Exposes REST API + MCP on a single port (default `:7878`) and a dashboard on `:7879`.

## Stack

Node.js 20+, TypeScript (strict, ESM via `"type": "module"`), Fastify, better-sqlite3, Zod, MCP SDK. No ORM.

## Commands

```bash
npm run dev          # tsx watch — runs `start` with hot reload
npm run build        # tsc + copy dashboard HTML + chmod bin
npm test             # vitest
npm run lint         # eslint (strict config)
npm run lint:fix     # eslint --fix
npm run format       # prettier --write
npm run check        # lint:fix + format + build (full pipeline)
npm run cli -- <cmd> # run CLI without building (e.g. npm run cli -- init)
```

Build copies `src/dashboard/index.html` to `dist/` — it's a single-file React app (CDN, no build step). The `prepare` script runs husky (not build).

## Pre-commit hooks

Husky + lint-staged runs on every commit:
- `*.{ts,js}` -> `eslint --fix` + `prettier --write`
- `*.json` -> `prettier --write`

## Source layout

```
src/
├── cli/          # CLI entry (init, start, connect, status)
├── core/         # db.ts (schema), models.ts (Zod), config.ts, memory.ts (tiers)
├── server/       # Fastify app + routes/ (health, workspace, conversations, entities, facts, search)
├── mcp/          # MCP HTTP server (mounted on same Fastify instance)
├── dashboard/    # Single HTML file served on port+1
└── fastify.d.ts  # Fastify type augmentation
```

- `cli/index.ts` is the bin entrypoint — dispatches subcommands via dynamic import.
- `core/db.ts` owns the SQLite schema and migrations. All tables live in one `.db` file.
- Routes are registered as Fastify plugins in `server/routes/`.
- `server/routes/health.ts` also serves `GET /connect` (client config endpoint).

## Runtime

- Config lives in `config.json` inside the data directory (default `~/.dbrain/`).
- CLI reads config path from argv, not env vars. Env vars (`DBRAIN_*`) are for Docker/non-interactive init only.
- Auth: single bearer token set during `init`. All endpoints except `/health` and `/connect` require it.
- Dashboard is a separate Fastify instance on `port + 1`.

## Conventions

- ESM imports require `.js` extension in source (NodeNext resolution).
- **Semicolons required** — prettier enforces `semi: true`.
- Single quotes, trailing commas, 100 char print width (see `.prettierrc`).
- `import type` preferred — eslint enforces `consistent-type-imports`.
- Import order enforced: builtin > external > internal > parent > sibling > index, alphabetized, with newlines between groups.
- `no-console` enforced except in `src/cli/` and `src/dashboard/server.ts`.
- `dist/` is gitignored.
