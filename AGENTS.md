# Tuning Studio development

The related firmware repository is `../../Embedded/rm-embedded-rs`.
Keep Tuning Studio integration changes there on `feat/tuning`; do not add them
to `refactor/boundary-cleanup`. Cherry-pick relevant existing firmware changes
with `-x`, and pin shared firmware dependencies to an immutable revision of this
repository until the corresponding crates.io versions are published.

Release automation is manually triggered. Build verification uses both
`create_release=false` and `publish_crates=false`; public publication is a
separate action.
