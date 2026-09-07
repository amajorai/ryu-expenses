<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="./icon-dark.png" />
    <img src="./icon-light.png" alt="Expenses" width="144" />
  </picture>
</p>

<div align="center">

# Expenses

</div>

A local-first expense ledger with an agent, Companion, MCP tools, and one SQLite source of truth.

> **The public home of `ryu-expenses`.** Source, builds, and releases live here —
> binaries for every platform are attached to each release.
>
> This tree is generated from the Ryu monorepo, so commits pushed here
> directly are replaced on the next sync. **Pull requests are welcome** —
> open them here and they are ported into the monorepo, then flow back out.
> Ryu as a whole: https://github.com/amajorai/ryu

## Install

**App:** [Install](ryu://apps/@ryu/expenses) (opens the Ryu desktop app and asks you to confirm)

**CLI:**

```bash
ryu apps add @ryu/expenses
```

**Crate:**

```bash
cargo install ryu-expenses
```

Prebuilt binaries for every platform are attached to [each release](https://github.com/amajorai/ryu/releases).

## License

Apache-2.0 — see [LICENSE](./LICENSE).

## Use it two ways

- Install the **Expenses** app to get the Expense Tracker agent runnable and the
  visual companion together.
- Install the standalone **Expense Tracker** Marketplace agent first, then add
  the Expenses app when you want the visual view. The agent template declares
  `@ryu/expenses` as its required data provider; it does not copy or create a
  second ledger.

## Agent tools

The sidecar's MCP server is registered as `expenses` and exposes:

- `expenses.list`
- `expenses.summary`
- `expenses.add`
- `expenses.update`
- `expenses.delete`

The companion calls the same store through the generic own-app bridge. It never
receives a node token or makes a direct network request.

## Data

Money is stored as positive integer minor units, dates are calendar strings, and
summaries never add unlike currencies. The node owns the SQLite database. This
app does not connect to banks, convert exchange rates, or provide financial or
tax advice.

## Local sidecar

The process is `ryu-expenses`. Core supplies its resolved data directory and
sidecar port when the app is enabled. For an isolated standalone development
run, use `RYU_PROFILE=dev RYU_KEYCHAIN=off RYU_DIR=/tmp/ryu-expenses-dev`.
