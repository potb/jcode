# Config persistence

`~/.jcode/config.toml` (or `$JCODE_HOME/config.toml`) holds only what the user
chose. Two mechanisms keep it that way, and they interact badly if either is
used alone.

## Pruning on save

`Config::save` writes `to_pruned_toml`, which omits every value still equal to
the shipped default. Without it a single save would write the entire schema into
the user's file, burying real overrides among hundreds of default lines and
freezing today's defaults forever, so a later improvement to a default would
never reach that user.

Pruning is safe because every field deserializes with `#[serde(default)]`, so an
omitted key round-trips to the same value.

## Environment overrides are per-run

`Config::load` applies `apply_env_overrides` on top of the file. That belongs to
the running process only: `JCODE_MODEL`, `JCODE_SESSION_FACTS`, and the rest are
how a user changes one run without editing their config.

`Config::load_for_edit` deliberately skips those overrides. Every
read-modify-write cycle must use it: `set_default_model`, `set_pin_usage`, the
`/colors` writer, the gateway toggle, and anything else that loads, patches one
field, and calls `save`.

## Why the two must not be combined

`load` followed by `save` is silently destructive, and the damage is worst in the
case that looks most harmless.

Take a user whose file says `session_facts = "left"` while the shipped default is
`"right"`. If `JCODE_SESSION_FACTS=right` is set in the environment, then:

1. `load` applies the override, so the in-memory value becomes `"right"`.
2. `save` prunes it, because it now equals the default.
3. The user's explicit `"left"` is gone from the file, permanently.

The user never asked for that. They set an env var for one run and lost a
persisted preference. The same path deletes whole structured sections such as
`[[ambient.projects]]` and `[[cron]]`, which cannot be reconstructed from
defaults at all.

The inverse leak is the milder half of the same bug: an override that differs
from the default gets written into the file and becomes permanent, so it keeps
applying long after the environment variable is gone.

## Adding a setter

Load with `load_for_edit`, patch the one field, then `save`. Never
`Config::load` on a path that saves.

`config_tests.rs` pins this: a setter must not persist an environment override,
must not let an override equal to a default erase the user's opposite value, and
must leave structured sections intact.
