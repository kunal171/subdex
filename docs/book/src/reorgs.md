# Reorgs & finality

The processor anchors on the chain's **finalized** head. Before committing a block
it validates that the block's `parent_hash` matches the hash stored for the
previous height:

- **Match** → commit normally (handler writes + cursor advance, atomically).
- **Mismatch** → a reorg replaced the parent. The processor walks **down** to the
  **true common ancestor** — comparing each stored hash against the source's
  canonical hash at that height — rolls back the diverged tail in **one** pass, and
  re-fetches from the fork point. The rewind is bounded by `MAX_REORG_DEPTH`
  (default 64; a deeper fork errors rather than rewinding unboundedly).

Because subdex indexes finalized blocks, deep reorgs are not expected; the
parent-hash check protects against any divergence within the retained window
(`REORG_RETENTION`). On GRANDPA chains the finalized cursor is clean and
unambiguous.

## Why walk to the common ancestor?

An earlier design assumed a fork was exactly one block back and self-corrected by
rewinding one block per engine iteration — one fetch-and-rollback round trip **per
block of depth**, with no bound. Walking straight to the real common ancestor and
rolling back once is both correct for arbitrary depth and far cheaper. The full
story is in [Design decisions](./design-decisions.md) and
[Data flow](./data-flow.md).
