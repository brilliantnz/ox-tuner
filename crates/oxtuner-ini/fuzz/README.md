# Fuzzing `oxtuner-ini`

Three targets cover the three entry points:

| Target        | Surface                                       |
| ------------- | --------------------------------------------- |
| `parse_ini`   | the full pipeline from UTF-8 text             |
| `parse_bytes` | the lossy decoder plus the full pipeline      |
| `preprocess`  | conditionals, `#define` expansion, `#include` |

Run them with `cargo +nightly fuzz run <target>`. Corpus and artifact folders
are git-ignored; seed the corpus with the reference definitions when they are
available locally:

```sh
mkdir -p corpus/parse_ini corpus/parse_bytes
cp ../../../docs/tmp/local/ini/*.ini corpus/parse_bytes/
```

The INIs are third-party files and are never committed (ADR-0006 decision 7).

The same contracts are covered without nightly by
`tests/properties.rs`, so a regression is caught in the normal CI run even
when nobody fuzzes.
