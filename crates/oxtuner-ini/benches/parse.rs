//! INI parser benchmarks: preprocess, full parse, `#define` expansion, and
//! expression evaluation.
//!
//! Run locally with:
//!
//! ```text
//! cargo bench -p oxtuner-ini
//! ```
//!
//! Three fixtures, each covering a different cost:
//!
//! - **small** — two pages, 400 scalars, a 16×16 table, channels, a menu. A
//!   smoke run that finishes fast and catches gross regressions.
//! - **reference shape** — fifteen pages and 818 constants, the shape of the
//!   reference project. Use this one when comparing against a start-up number.
//! - **defines** — a single 65 536-field `#define`, the CAN address table a
//!   real definition spells out. It guards the memoized [`Expander`]: expanding
//!   that list once per reference made `[Constants]` dominate the whole parse.
//!
//! The fixtures are synthetic. Never commit a real vendor INI here; the
//! clean-room rule applies to benchmarks too.

use std::fmt::Write as _;
use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use oxtuner_ini::{Ordered, SettingsContext, expr::Expr, parse_str, preprocess::preprocess};

/// Two pages, 400 scalars, one 16x16 table, four channels, one menu.
fn fixture() -> String {
    let mut text = String::from(
        "\
[MegaTune]
signature = \"bench 1\"
queryCommand = \"Q\"

[Constants]
nPages = 2
pageSize = 288, 288
",
    );

    for page in 1..=2 {
        let _ = writeln!(text, "page = {page}");

        for index in 0..200 {
            let _ = writeln!(
                text,
                "scalar_{page}_{index} = scalar, U08, {}, \"\", 1, 0, 0, 255, 0",
                index % 240
            );
        }
    }

    text.push_str(
        "\
table_bins = array, U08, 0, [16], \"\", 1, 0, 0, 255, 0
table_cells = array, U08, 16, [16x16], \"\", 1, 0, 0, 255, 0

[OutputChannels]
rpm = scalar, U16, 0, \"RPM\", 1.0, 0.0
map = scalar, U16, 2, \"kPa\", 0.1, 0.0
afr = { ego1 / 10.0 }, \"AFR\"
clt = scalar, U16, 4, \"C\", 0.1, 0.0

[TableEditor]
table = veTable, veMap, \"VE Table\"
xBins = table_bins, rpm
yBins = table_bins, map
zBins = table_cells

[Menu]
menu = \"Fuel\"
subMenu = veTable, \"VE Table\"
std_separator
",
    );

    text
}

/// Fifteen pages, 818 constants, eight tables: the budget's own shape.
fn reference_fixture() -> String {
    const PAGES: usize = 15;
    const CONSTANTS: usize = 818;
    const TABLES: usize = 8;

    let scalars_per_page = (CONSTANTS - TABLES) / PAGES;
    let mut text = String::from(
        "\
[MegaTune]
signature = \"speeduino 202501\"
queryCommand = \"Q\"

[Constants]
nPages = 15
",
    );

    for _ in 0..PAGES {
        text.push_str("pageSize = 288\n");
    }

    for page in 1..=PAGES {
        let _ = writeln!(text, "page = {page}");

        for index in 0..scalars_per_page {
            let _ = writeln!(
                text,
                "scalar_{page}_{index} = scalar, U08, {}, \"\", 1, 0, 0, 255, 0",
                index % 240
            );
        }

        if page <= TABLES {
            // The largest grid the editor draws; the other seven are 16x16.
            let shape = if page == 1 { "[32x32]" } else { "[16x16]" };
            let _ = writeln!(
                text,
                "table_{page} = array, U08, 256, {shape}, \"\", 1, 0, 0, 255, 0"
            );
        }
    }

    text.push_str(
        "\
[OutputChannels]
rpm = scalar, U16, 0, \"RPM\", 1.0, 0.0
map = scalar, U16, 2, \"kPa\", 0.1, 0.0
clt = scalar, U16, 4, \"C\", 0.1, 0.0
afr = scalar, U16, 6, \"AFR\", 0.1, 0.0

[Menu]
menu = \"Fuel\"
subMenu = table_1, \"VE Table\"
menu = \"Ignition\"
subMenu = table_2, \"Advance\"
",
    );

    text
}

/// One 65 536-field `#define`, the CAN address table in the speeduino shape.
fn defines_fixture() -> String {
    const ADDRESSES: usize = 65_536;

    let mut addresses = String::with_capacity(ADDRESSES * 8);
    for index in 0..ADDRESSES {
        if index > 0 {
            addresses.push_str(", ");
        }
        let _ = write!(addresses, "\"0x{index:x}\"");
    }

    format!(
        "\
[MegaTune]
signature = \"bench 1\"
queryCommand = \"Q\"

[Constants]
nPages = 1
pageSize = 288
page = 1
#define canAddresses = {addresses}
"
    )
}

/// The option-list and PC-variable shapes the parser gained compatibility for:
/// quoted labels containing `=`, a period-separated label list, and scaled PC
/// arrays.
fn options_fixture() -> String {
    let mut text = String::from(
        "\
[MegaTune]
signature = \"bench 1\"
queryCommand = \"Q\"

[Constants]
nPages = 1
pageSize = 512
page = 1
compare = bits, U08, 0, [0:7], ",
    );

    for index in 0..64 {
        if index > 0 {
            text.push_str(", ");
        }
        let _ = write!(text, "\"== option {index}\"");
    }

    text.push_str("\nserial = bits, U08, 1, [0:3], \"One\". \"Two\". \"Three\". \"Four\"\n");
    text.push_str("\n[PcVariables]\n");

    for index in 0..200 {
        let _ = writeln!(
            text,
            "bins_{index} = array, S16, [10], \"AFR\", 0.1, 0.0, -4.0, 4.0, 1"
        );
    }

    text
}

fn bench_parse_options(criterion: &mut Criterion) {
    let text = options_fixture();
    let settings = SettingsContext::new();

    criterion.bench_function("ini_parse_bit_options_and_pc_variables", |bencher| {
        bencher
            .iter(|| black_box(parse_str(black_box(&text), black_box(&settings)).expect("parses")));
    });
}

fn bench_preprocess(criterion: &mut Criterion) {
    let text = fixture();
    let settings = SettingsContext::new();

    criterion.bench_function("ini_preprocess_synthetic", |bencher| {
        bencher.iter(|| {
            black_box(
                preprocess(black_box(&text), black_box(&settings), None).expect("preprocesses"),
            )
        });
    });
}

fn bench_parse_str(criterion: &mut Criterion) {
    let text = fixture();
    let settings = SettingsContext::new();

    criterion.bench_function("ini_parse_synthetic", |bencher| {
        bencher
            .iter(|| black_box(parse_str(black_box(&text), black_box(&settings)).expect("parses")));
    });
}

fn bench_parse_bytes(criterion: &mut Criterion) {
    let text = fixture();
    let bytes = text.into_bytes();
    let settings = SettingsContext::new();

    criterion.bench_function("ini_parse_bytes_synthetic", |bencher| {
        bencher.iter(|| {
            black_box(
                oxtuner_ini::parse_bytes(black_box(&bytes), black_box(&settings)).expect("parses"),
            )
        });
    });
}

fn bench_parse_reference_shape(criterion: &mut Criterion) {
    let text = reference_fixture();
    let settings = SettingsContext::new();

    criterion.bench_function("ini_parse_reference_shape", |bencher| {
        bencher
            .iter(|| black_box(parse_str(black_box(&text), black_box(&settings)).expect("parses")));
    });
}

fn bench_preprocess_defines(criterion: &mut Criterion) {
    let text = defines_fixture();
    let settings = SettingsContext::new();

    criterion.bench_function("ini_preprocess_defines_65536", |bencher| {
        bencher.iter(|| {
            black_box(
                preprocess(black_box(&text), black_box(&settings), None).expect("preprocesses"),
            )
        });
    });
}

fn bench_expander_build(criterion: &mut Criterion) {
    let text = defines_fixture();
    let settings = SettingsContext::new();
    let preprocessed = preprocess(&text, &settings, None).expect("preprocesses");

    criterion.bench_function("ini_expander_build_65536", |bencher| {
        bencher.iter(|| black_box(black_box(&preprocessed).expander()));
    });
}

fn bench_expand_reference(criterion: &mut Criterion) {
    let text = defines_fixture();
    let settings = SettingsContext::new();
    let preprocessed = preprocess(&text, &settings, None).expect("preprocesses");
    let expander = preprocessed.expander();
    let fields = ["$canAddresses"];

    criterion.bench_function("ini_expand_one_reference_65536", |bencher| {
        bencher.iter(|| black_box(expander.expand(black_box(&fields))));
    });
}

fn bench_expr(criterion: &mut Criterion) {
    let expression = Expr::parse("ego1 / 10.0 + clt * 2.0").expect("parses");
    let lookup = |name: &str| match name {
        "ego1" => Some(147.0),
        "clt" => Some(80.0),
        _ => None,
    };

    criterion.bench_function("ini_expr_evaluate", |bencher| {
        bencher.iter(|| black_box(expression.evaluate(black_box(&lookup))));
    });
}

/// A name-keyed collection the size of the reference definition's constants.
fn ordered_constants(count: usize) -> Ordered<usize> {
    let mut constants = Ordered::new();
    for index in 0..count {
        constants.insert(format!("constant_{index}"), index);
    }
    constants
}

/// `Definition::constant`/`channel`/`table` all go through `get_ignore_case`, so a
/// resolution pass makes this lookup once per reference in the file.
fn bench_ordered_lookup(criterion: &mut Criterion) {
    let constants = ordered_constants(818);
    let small = ordered_constants(8);

    criterion.bench_function("ini_ordered_get_ci_exact", |bencher| {
        bencher.iter(|| black_box(constants.get_ignore_case(black_box("constant_400"))));
    });
    criterion.bench_function("ini_ordered_get_ci_mixed_case", |bencher| {
        bencher.iter(|| black_box(constants.get_ignore_case(black_box("CoNsTaNt_400"))));
    });
    criterion.bench_function("ini_ordered_get_ci_missing", |bencher| {
        bencher.iter(|| black_box(constants.get_ignore_case(black_box("missing_400"))));
    });
    criterion.bench_function("ini_ordered_get_ci_small_mixed_case", |bencher| {
        bencher.iter(|| black_box(small.get_ignore_case(black_box("CoNsTaNt_4"))));
    });
}

criterion_group!(
    benches,
    bench_preprocess,
    bench_parse_str,
    bench_parse_bytes,
    bench_parse_reference_shape,
    bench_parse_options,
    bench_preprocess_defines,
    bench_expander_build,
    bench_expand_reference,
    bench_expr,
    bench_ordered_lookup
);
criterion_main!(benches);
