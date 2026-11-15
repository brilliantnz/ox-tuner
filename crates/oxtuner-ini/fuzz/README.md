# Fuzzing `oxtuner-ini`

Five targets: three entry points and two that keep the input on a single
entry line, where the full pipeline would rarely generate a valid one.

| Target         | Surface                                         |
| -------------- | ----------------------------------------------- |
| `parse_ini`    | the full pipeline from UTF-8 text               |
| `parse_bytes`  | the lossy decoder plus the full pipeline        |
| `preprocess`   | conditionals, `#define` expansion, `#include`   |
| `bit_options`  | a `bits` option list: labels, `=`, `".`         |
| `pc_variables` | one `[PcVariables]` entry: shape and attributes |

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
