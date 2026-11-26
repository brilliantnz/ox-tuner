# Fuzzing `oxtuner-ini`

This isolated cargo-fuzz workspace exercises the parser and preprocessor on
arbitrary input. It has six targets: three full-pipeline entry points and
three focused parsers that accept a single entry or section.

| Target         | Input surface                                   |
| -------------- | ----------------------------------------------- |
| `parse_ini`    | UTF-8 text through the full parse pipeline      |
| `parse_bytes`  | arbitrary bytes through decoding and parsing    |
| `preprocess`   | conditionals, definitions, and include handling |
| `bit_options`  | one bit-option list                             |
| `pc_variables` | one `[PcVariables]` entry                       |
| `sections`     | logger fields and named configuration sections  |

Run a target from the `crates/oxtuner-ini` directory with a nightly toolchain
and `cargo-fuzz` installed:

```sh
cargo +nightly fuzz run parse_ini -- -dict=fuzz/ini.dict
```

The dictionary helps mutations reach section dispatch. Corpus and crash
artifacts are local fuzzing data and should not be committed. Do not copy real
definition files into the repository.

The parser's fixture and property tests run without nightly:

```sh
cargo nextest run -p oxtuner-ini
```
