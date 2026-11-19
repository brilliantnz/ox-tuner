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
//! The newer shapes cover the added sections and passes: logger records, named
//! config sections, deferred-call validation, repeated headers, an include
//! chain, nested conditionals, builtin lookup and expression normalisation.
//!
//! The vendor-scale shapes mirror the local-only real files in proportion
//! only, never in text: a Speeduino-like shape near 500 kB with one large
//! shared `#define` list, a dialog-heavy shape, a mid-size logger-plus-config
//! shape, and one wide-table shape that stresses peak temporary memory. The
//! cached-load bench round-trips a `Definition` through postcard, the same
//! frozen-cache encoding the project cache stores, against the 15 ms budget.
//!
//! `Definition` size class (entry counts plus the shared bit-option table
//! factor) prints once per bench setup, next to the raw-parse and cached-load
//! times. The wide-table bench also asserts peak temporary memory: under
//! 10 MB passes clean, 10-30 MB passes in range, above 30 MB fails the bench.
//! Benches never gate CI; every number stays machine-relative.
//!
//! The fixtures are synthetic. Never commit a real vendor INI here; the
//! clean-room rule applies to benchmarks too.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs;
use std::hint::black_box;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use criterion::{Criterion, criterion_group, criterion_main};
use oxtuner_ini::{
    BitOption, ConstantClass, Definition, Number, Ordered, SettingsContext, builtins, expr::Expr,
    parse_str, parse_str_with_includes, preprocess::preprocess,
};

/// Peak temporary memory for the wide-table bench.
///
/// The library must not change, so the tracker lives here in the bench binary:
/// a delegating global allocator that records the high-water mark of live
/// bytes. The wide bench resets the mark around one probe parse and asserts
/// the delta against the 10 MB aim and the 30 MB limit.
///
/// Live heap bytes allocated through this binary's global allocator.
static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
/// High-water mark of [`LIVE_BYTES`]; reset to live bytes before each probe.
static PEAK_BYTES: AtomicUsize = AtomicUsize::new(0);
/// Live bytes when the current probe started, so the delta is temporary use.
static PROBE_BASE_BYTES: AtomicUsize = AtomicUsize::new(0);

/// Under 10 MB of temporary heap passes the wide bench clean.
const PEAK_CLEAN_BYTES: usize = 10 * 1024 * 1024;
/// Above 30 MB of temporary heap fails the wide bench (bench-only, never CI).
const PEAK_LIMIT_BYTES: usize = 30 * 1024 * 1024;

struct PeakAllocator;

#[expect(
    unsafe_code,
    reason = "bench-only peak tracker; the library keeps using `System` directly"
)]
// SAFETY: every call delegates to `System` with the caller's own layout and
// pointer, so ownership and alignment follow the global allocator contract.
unsafe impl GlobalAlloc for PeakAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: `layout` comes from the caller, and the pointer (or null)
        // goes back to the caller untouched, so the allocator contract holds.
        #[expect(unsafe_code, reason = "bench-only peak tracker delegates to `System`")]
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            let live = LIVE_BYTES.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK_BYTES.fetch_max(live, Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        LIVE_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: `pointer` and `layout` are the pair the caller allocated,
        // passed straight through to `System`.
        #[expect(unsafe_code, reason = "bench-only peak tracker delegates to `System`")]
        unsafe {
            System.dealloc(pointer, layout);
        }
    }
}

#[global_allocator]
static GLOBAL_ALLOCATOR: PeakAllocator = PeakAllocator;

/// Points the high-water mark at current live bytes before a probe parse.
fn reset_peak() {
    let live = LIVE_BYTES.load(Ordering::Relaxed);
    PROBE_BASE_BYTES.store(live, Ordering::Relaxed);
    PEAK_BYTES.store(live, Ordering::Relaxed);
}

/// Temporary bytes allocated during the probe: high-water mark minus live
/// bytes at probe start.
fn probe_peak_bytes() -> usize {
    PEAK_BYTES
        .load(Ordering::Relaxed)
        .saturating_sub(PROBE_BASE_BYTES.load(Ordering::Relaxed))
}

/// Bit-option references over distinct shared tables: the sharing factor the
/// `Arc` option lists buy.
fn shared_option_stats(definition: &Definition) -> (usize, usize) {
    let mut lists: HashSet<*const [BitOption]> = HashSet::new();
    let mut refs = 0;

    for (_, constant) in definition.constants.iter() {
        if constant.class == ConstantClass::Bits {
            refs += 1;
            lists.insert(Arc::as_ptr(&constant.options));
        }
    }

    for (_, variable) in definition.pc_variables.iter() {
        if variable.class == ConstantClass::Bits {
            refs += 1;
            lists.insert(Arc::as_ptr(&variable.options));
        }
    }

    (refs, lists.len())
}

/// One-line size class for bench output: entry counts plus the shared
/// bit-option table factor.
fn size_class(definition: &Definition) -> String {
    let (option_refs, option_lists) = shared_option_stats(definition);

    format!(
        "pages={} constants={} pc_vars={} channels={} tables={} curves={} gauges={} \
         dialogs={} menus={} datalog={} loggers={} config_sections={} defines={} \
         options={option_refs}/{option_lists} diagnostics={}",
        definition.pages.len(),
        definition.constants.len(),
        definition.pc_variables.len(),
        definition.channels.len(),
        definition.tables.len(),
        definition.curves.len(),
        definition.gauges.len(),
        definition.dialogs.len(),
        definition.menus.len(),
        definition.datalog_entries.len(),
        definition.loggers.len(),
        definition.config_sections.len(),
        definition.defines.len(),
        definition.diagnostics.len()
    )
}

#[expect(
    clippy::print_stdout,
    reason = "bench output carries the size class per the performance plan"
)]
fn report_shape(label: &str, text: &str, definition: &Definition) {
    println!(
        "{label}: {} bytes of INI, {}",
        text.len(),
        size_class(definition)
    );
}

#[expect(
    clippy::print_stdout,
    reason = "bench output carries the frozen-cache size per the performance plan"
)]
fn report_frozen(label: &str, bytes: usize) {
    println!("{label}: {bytes} bytes of frozen cache");
}

#[expect(
    clippy::print_stdout,
    reason = "bench output carries the peak-temporary class per the performance plan"
)]
fn report_peak(label: &str, peak: usize) {
    if peak <= PEAK_CLEAN_BYTES {
        println!("{label}: peak temporary {peak} bytes, under the 10 MB aim: clean");
    } else {
        println!("{label}: peak temporary {peak} bytes, in the 10-30 MB range");
    }
}

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

/// Vendor-scale shape near 500 kB: fifteen pages, fifteen hundred constants,
/// eight tables, two hundred channels, a small dialog set, and one 32 768-label
/// `#define` list carrying most of the bytes. Two `bits` entries share that
/// list through one `Arc` table. Names and labels are invented; nothing here is
/// vendor text.
fn speeduino_shape_fixture() -> String {
    const PAGES: usize = 15;
    const CONSTANTS_PER_PAGE: usize = 100;
    const TABLES: usize = 8;
    const CHANNELS: usize = 200;
    const ADDRESSES: usize = 32_768;

    let mut text = String::with_capacity(520_000);
    text.push_str(
        "\
[MegaTune]
signature = \"bench speeduino shape\"
queryCommand = \"Q\"

[Constants]
nPages = 15
",
    );

    for _ in 0..PAGES {
        text.push_str("pageSize = 4096\n");
    }

    for page in 1..=PAGES {
        let _ = writeln!(text, "page = {page}");

        for index in 0..CONSTANTS_PER_PAGE {
            let _ = writeln!(
                text,
                "spd_{page}_{index} = scalar, U08, {}, \"\", 1, 0, 0, 255, 0",
                index % 240
            );
        }

        if page <= TABLES {
            let shape = if page == 1 { "[32x32]" } else { "[16x16]" };
            let _ = writeln!(
                text,
                "spd_table_{page} = array, U08, 1024, {shape}, \"\", 1, 0, 0, 255, 0"
            );
        }
    }

    text.push_str("#define benchAddresses = ");

    for index in 0..ADDRESSES {
        if index > 0 {
            text.push_str(", ");
        }
        let _ = write!(text, "\"opt_{index}\"");
    }

    text.push_str(
        "\naddrLow = bits, U16, 0, [0:15], $benchAddresses\naddrHigh = bits, U16, 2, [0:15], $benchAddresses\n",
    );
    text.push_str("\n[OutputChannels]\n");

    for index in 0..CHANNELS {
        let _ = writeln!(
            text,
            "spd_chan_{index} = scalar, U16, {}, \"units\", 0.1, 0.0",
            index * 2
        );
    }

    text.push_str(
        "\n[TableEditor]\ntable = spd_table_1, spd_table_1, \"Bench Table\"\nxBins = spd_table_2, spd_chan_0\nyBins = spd_table_2, spd_chan_1\nzBins = spd_table_1\n\n[Menu]\nmenu = \"Fuel\"\nsubMenu = spd_table_1, \"Bench Table\"\n",
    );

    text
}

/// Dialog-heavy vendor-scale shape: twenty pages, two thousand constants,
/// sixteen tables, one hundred fifty channels, and thirty dialogs. Names are
/// invented; only the proportions mirror a dialog-heavy real shape.
fn dialog_shape_fixture() -> String {
    const PAGES: usize = 20;
    const CONSTANTS_PER_PAGE: usize = 100;
    const TABLES: usize = 16;
    const CHANNELS: usize = 150;
    const DIALOGS: usize = 30;
    const FIELDS_PER_DIALOG: usize = 12;

    let mut text = String::with_capacity(280_000);
    text.push_str(
        "\
[MegaTune]
signature = \"bench dialog shape\"
queryCommand = \"Q\"

[Constants]
nPages = 20
",
    );

    for _ in 0..PAGES {
        text.push_str("pageSize = 4096\n");
    }

    for page in 1..=PAGES {
        let _ = writeln!(text, "page = {page}");

        for index in 0..CONSTANTS_PER_PAGE {
            let _ = writeln!(
                text,
                "rus_{page}_{index} = scalar, U08, {}, \"\", 1, 0, 0, 255, 0",
                index % 240
            );
        }

        if page <= TABLES {
            let shape = if page == 1 { "[32x32]" } else { "[16x16]" };
            let _ = writeln!(
                text,
                "rus_table_{page} = array, U08, 1024, {shape}, \"\", 1, 0, 0, 255, 0"
            );
        }
    }

    text.push_str("\n[OutputChannels]\n");

    for index in 0..CHANNELS {
        let _ = writeln!(
            text,
            "rus_chan_{index} = scalar, U16, {}, \"units\", 0.1, 0.0",
            index * 2
        );
    }

    text.push_str("\n[UiDialogs]\n");

    for dialog in 0..DIALOGS {
        let _ = writeln!(text, "dialog = dlg_{dialog}, \"Dialog {dialog}\", yAxis");

        for index in 0..FIELDS_PER_DIALOG {
            let _ = writeln!(text, "field = \"F{index}\", rus_1_{index}");
        }
    }

    text.push_str("\n[Menu]\nmenu = \"Tuning\"\nsubMenu = rus_table_1, \"Bench Table\"\n");

    text
}

/// Mid-size vendor-scale shape: sixteen pages, twelve hundred constants,
/// twelve tables, one hundred channels, one composite logger, and two named
/// config sections. Names are invented.
fn config_shape_fixture() -> String {
    const PAGES: usize = 16;
    const CONSTANTS_PER_PAGE: usize = 75;
    const TABLES: usize = 12;
    const CHANNELS: usize = 100;
    const LOGGER_FIELDS: usize = 64;

    let mut text = String::with_capacity(200_000);
    text.push_str(
        "\
[MegaTune]
signature = \"bench mid shape\"
queryCommand = \"Q\"

[Constants]
nPages = 16
",
    );

    for _ in 0..PAGES {
        text.push_str("pageSize = 4096\n");
    }

    for page in 1..=PAGES {
        let _ = writeln!(text, "page = {page}");

        for index in 0..CONSTANTS_PER_PAGE {
            let _ = writeln!(
                text,
                "fom_{page}_{index} = scalar, U08, {}, \"\", 1, 0, 0, 255, 0",
                index % 240
            );
        }

        if page <= TABLES {
            let shape = if page == 1 { "[32x32]" } else { "[16x16]" };
            let _ = writeln!(
                text,
                "fom_table_{page} = array, U08, 1024, {shape}, \"\", 1, 0, 0, 255, 0"
            );
        }
    }

    text.push_str("\n[OutputChannels]\n");

    for index in 0..CHANNELS {
        let _ = writeln!(
            text,
            "fom_chan_{index} = scalar, U16, {}, \"units\", 0.1, 0.0",
            index * 2
        );
    }

    text.push_str(
        "\n[LoggerDefinition]\nloggerDef = benchLogger, \"Bench\", composite\nrecordDef = 0, 0, 8\n",
    );

    for index in 0..LOGGER_FIELDS {
        let _ = writeln!(
            text,
            "recordField = fom_field_{index}, \"Field {index}\", {}, 1, 1.0, \"Flag\"",
            index * 8
        );
    }

    for section in ["PortEditor", "VeAnalyze"] {
        let _ = writeln!(text, "\n[{section}]");

        for index in 0..40 {
            let _ = writeln!(
                text,
                "entry_{index} = \"{section}\", {index}, {{ fom_chan_0 > {index} }}"
            );
        }
    }

    text
}

/// Wide-table shape for the peak-temporary check: two pages of one thousand
/// constants each, four 32x32 tables, a 128-field logger record, and an
/// 8 192-label `#define` shared by two entries. Names are invented.
fn wide_table_fixture() -> String {
    const PAGES: usize = 2;
    const CONSTANTS_PER_PAGE: usize = 1000;
    const LOGGER_FIELDS: usize = 128;
    const ADDRESSES: usize = 8192;

    let mut text = String::with_capacity(300_000);
    text.push_str(
        "\
[MegaTune]
signature = \"bench wide shape\"
queryCommand = \"Q\"

[Constants]
nPages = 2
pageSize = 4096
pageSize = 4096
page = 1
",
    );

    for page in 1..=PAGES {
        if page > 1 {
            text.push_str("page = 2\n");
        }

        for index in 0..CONSTANTS_PER_PAGE {
            let _ = writeln!(
                text,
                "wide_{page}_{index} = scalar, U08, {index}, \"\", 1, 0, 0, 255, 0"
            );
        }

        let _ = writeln!(
            text,
            "wide_table_{page}_a = array, U08, 1024, [32x32], \"\", 1, 0, 0, 255, 0"
        );
        let _ = writeln!(
            text,
            "wide_table_{page}_b = array, U08, 2048, [32x32], \"\", 1, 0, 0, 255, 0"
        );
    }

    text.push_str("#define wideAddresses = ");

    for index in 0..ADDRESSES {
        if index > 0 {
            text.push_str(", ");
        }
        let _ = write!(text, "\"wide_{index}\"");
    }

    text.push_str(
        "\nwideLow = bits, U16, 3072, [0:12], $wideAddresses\nwideHigh = bits, U16, 3074, [0:12], $wideAddresses\n",
    );
    text.push_str(
        "\n[LoggerDefinition]\nloggerDef = benchWide, \"Bench\", composite\nrecordDef = 0, 0, 8\n",
    );

    for index in 0..LOGGER_FIELDS {
        let _ = writeln!(
            text,
            "recordField = wide_field_{index}, \"Field {index}\", {}, 1, 1.0, \"Flag\"",
            index * 8
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

/// A logger with a wide bit-level record and several derived fields.
fn logger_fixture() -> String {
    let mut text = String::from(
        "\
[MegaTune]
signature = \"bench logger\"
[LoggerDefinition]
loggerDef = compositeLogger, \"Composite\", composite
startCommand = \"J\"
stopCommand = \"j\"
dataReadCommand = \"T\"
dataReadyCondition = { toothLogReady }
recordDef = 0, 0, 8
",
    );

    for index in 0..64 {
        let _ = writeln!(
            text,
            "recordField = field_{index}, \"Field {index}\", {}, 1, 1.0, \"Flag\"",
            index * 8
        );
    }
    for index in 0..8 {
        let _ = writeln!(
            text,
            "calcField = calc_{index}, \"Calc {index}\", \"ms\", {{ maxValue(refTime) }}"
        );
    }

    text
}

/// Named configuration sections from the post-v1 modelling work.
fn config_sections_fixture() -> String {
    let mut text = String::from("[MegaTune]\nsignature = \"bench config\"\n");

    for section in ["PortEditor", "FTPBrowser", "VeAnalyze", "DatalogViews"] {
        let _ = writeln!(text, "[{section}]");
        for index in 0..40 {
            let _ = writeln!(
                text,
                "entry_{index} = \"{section}\", {index}, {{ rpm > {index} }}"
            );
        }
    }

    text
}

/// Expressions that the builtin registry keeps symbolic, for the validation
/// pass that streams every frozen expression once at finalize.
fn deferred_calls_fixture() -> String {
    let mut text = String::from(
        "\
[MegaTune]
signature = \"bench calls\"
[Constants]
nPages = 1
pageSize = 512
page = 1
",
    );

    for index in 0..200 {
        let _ = writeln!(
            text,
            "c_{index} = scalar, U08, 0, {{ bitStringValue(units, algo) }}, \
             {{ someFutureBuiltin(1) }}, 0, 0, 255, 1"
        );
    }

    text
}

/// Repeated headers that must merge into one builder.
fn repeated_sections_fixture() -> String {
    let mut text = String::from("[MegaTune]\nsignature = \"bench merge\"\n");

    for block in 0..4 {
        text.push_str("[Constants]\nnPages = 1\npageSize = 256\npage = 1\n");
        for index in 0..40 {
            let _ = writeln!(
                text,
                "c_{block}_{index} = scalar, U08, {}, \"\", 1, 0, 0, 255, 0",
                index % 240
            );
        }
        text.push_str("[UiDialogs]\ndialog = d, \"D\", yAxis\n");
        for index in 0..10 {
            let _ = writeln!(text, "field = \"F{index}\", c_{block}_{index}");
        }
    }

    text
}

/// A three-deep include chain resolved through the public resolver entry.
fn include_chain_fixture() -> &'static str {
    "[MegaTune]\nsignature = \"bench include\"\n#include a.inc\n"
}

/// Nested conditional frames, with an `#else` branch on each.
fn conditional_frames_fixture() -> String {
    let mut text = String::from("[Constants]\n");

    for index in 0..100 {
        let _ = writeln!(text, "#if FLAG_{index}");
        let _ = writeln!(text, "a_{index} = 1");
        let _ = writeln!(text, "#else");
        let _ = writeln!(text, "b_{index} = 2");
        let _ = writeln!(text, "#endif");
    }

    text
}

fn bench_new_sections(criterion: &mut Criterion) {
    let settings = SettingsContext::new();

    let logger = logger_fixture();
    criterion.bench_function("ini_parse_logger_definition", |bencher| {
        bencher.iter(|| {
            black_box(parse_str(black_box(&logger), black_box(&settings)).expect("parses"))
        });
    });

    let config = config_sections_fixture();
    criterion.bench_function("ini_parse_config_sections", |bencher| {
        bencher.iter(|| {
            black_box(parse_str(black_box(&config), black_box(&settings)).expect("parses"))
        });
    });
}

fn bench_deferred_validation(criterion: &mut Criterion) {
    let text = deferred_calls_fixture();
    let settings = SettingsContext::new();

    criterion.bench_function("ini_parse_with_deferred_calls", |bencher| {
        bencher
            .iter(|| black_box(parse_str(black_box(&text), black_box(&settings)).expect("parses")));
    });
}

fn bench_builtin_registry(criterion: &mut Criterion) {
    criterion.bench_function("ini_builtin_lookup_and_fold", |bencher| {
        bencher.iter(|| {
            let string_function = black_box(builtins::lookup(black_box("bitStringValue")));
            let folded = black_box(builtins::fold(black_box("min"), black_box(&[3.0, 1.0])));
            black_box((string_function.map(|builtin| builtin.kind), folded));
        });
    });
}

fn bench_number_normalized(criterion: &mut Criterion) {
    let corpus: Vec<Number> = [
        "2",
        "{ 1 + 1 }",
        "{ min(a, b) }",
        "{ fuel_cols * 2 }",
        "{ bitStringValue(units, algo) }",
    ]
    .iter()
    .map(|source| Number::parse(source))
    .collect();

    criterion.bench_function("ini_number_normalized", |bencher| {
        bencher.iter(|| {
            for number in &corpus {
                black_box(number.normalized());
            }
        });
    });
}

fn bench_repeated_sections(criterion: &mut Criterion) {
    let text = repeated_sections_fixture();
    let settings = SettingsContext::new();

    criterion.bench_function("ini_parse_repeated_sections", |bencher| {
        bencher
            .iter(|| black_box(parse_str(black_box(&text), black_box(&settings)).expect("parses")));
    });
}

fn bench_include_chain(criterion: &mut Criterion) {
    let text = include_chain_fixture();
    let settings = SettingsContext::new();
    let resolver = |path: &str| match path {
        "a.inc" => Some("#include b.inc\n".to_owned()),
        "b.inc" => Some("#include c.inc\n".to_owned()),
        "c.inc" => Some("deep = 1\n".to_owned()),
        _ => None,
    };

    criterion.bench_function("ini_parse_include_chain", |bencher| {
        bencher.iter(|| {
            black_box(
                parse_str_with_includes(
                    black_box(text),
                    black_box(&settings),
                    black_box(&resolver),
                )
                .expect("parses"),
            )
        });
    });
}

fn bench_conditional_frames(criterion: &mut Criterion) {
    let text = conditional_frames_fixture();
    let settings = SettingsContext::new();

    criterion.bench_function("ini_preprocess_conditional_frames", |bencher| {
        bencher.iter(|| {
            black_box(
                preprocess(black_box(&text), black_box(&settings), None).expect("preprocesses"),
            )
        });
    });
}

fn bench_parse_speeduino_shape(criterion: &mut Criterion) {
    let text = speeduino_shape_fixture();
    let settings = SettingsContext::new();
    let definition = parse_str(&text, &settings).expect("parses");
    report_shape("ini_parse_speeduino_shape", &text, &definition);

    criterion.bench_function("ini_parse_speeduino_shape", |bencher| {
        bencher
            .iter(|| black_box(parse_str(black_box(&text), black_box(&settings)).expect("parses")));
    });
}

fn bench_parse_dialog_shape(criterion: &mut Criterion) {
    let text = dialog_shape_fixture();
    let settings = SettingsContext::new();
    let definition = parse_str(&text, &settings).expect("parses");
    report_shape("ini_parse_dialog_shape", &text, &definition);

    criterion.bench_function("ini_parse_dialog_shape", |bencher| {
        bencher
            .iter(|| black_box(parse_str(black_box(&text), black_box(&settings)).expect("parses")));
    });
}

fn bench_parse_config_shape(criterion: &mut Criterion) {
    let text = config_shape_fixture();
    let settings = SettingsContext::new();
    let definition = parse_str(&text, &settings).expect("parses");
    report_shape("ini_parse_mid_shape", &text, &definition);

    criterion.bench_function("ini_parse_mid_shape", |bencher| {
        bencher
            .iter(|| black_box(parse_str(black_box(&text), black_box(&settings)).expect("parses")));
    });
}

/// Wide-table parse plus the peak-temporary assertion.
///
/// The probe parse runs once outside the measurement with the high-water mark
/// reset, so `probe_peak_bytes` is the temporary heap of one parse. Under
/// 10 MB passes clean, 10-30 MB passes in range, above 30 MB fails the bench.
/// The limit is bench-only: benches never gate CI.
fn bench_parse_wide_tables(criterion: &mut Criterion) {
    let text = wide_table_fixture();
    let settings = SettingsContext::new();
    let definition = parse_str(&text, &settings).expect("parses");
    report_shape("ini_parse_wide_tables", &text, &definition);

    reset_peak();
    let probe = parse_str(&text, &settings).expect("parses");
    black_box(&probe);
    let peak = probe_peak_bytes();
    report_peak("ini_parse_wide_tables", peak);
    assert!(
        peak <= PEAK_LIMIT_BYTES,
        "wide-table peak temporary {peak} bytes exceeds the 30 MB bench limit"
    );

    criterion.bench_function("ini_parse_wide_tables", |bencher| {
        bencher
            .iter(|| black_box(parse_str(black_box(&text), black_box(&settings)).expect("parses")));
    });
}

/// Frozen-cache load through postcard, the encoding the definition cache
/// stores, against the same 15 ms budget as the raw parse. Setup parses one
/// Speeduino-shape text and serializes it once; the bench measures
/// `from_bytes` only. Bench public API only.
fn bench_cached_load(criterion: &mut Criterion) {
    let text = speeduino_shape_fixture();
    let settings = SettingsContext::new();
    let definition = parse_str(&text, &settings).expect("parses");
    report_shape("ini_cached_load_postcard", &text, &definition);
    let bytes = postcard::to_allocvec(&definition).expect("serializes");
    report_frozen("ini_cached_load_postcard", bytes.len());

    criterion.bench_function("ini_cached_load_postcard", |bencher| {
        bencher.iter(|| {
            let restored: Definition =
                postcard::from_bytes(black_box(&bytes)).expect("deserializes");
            black_box(restored);
        });
    });
}

/// A/B protocol: one temporary bench per variant (`ini_ab_<topic>_<variant>`),
/// 3 runs on the same machine, take the median. Save the baseline with
/// `cargo bench -p oxtuner-ini -- --save-baseline ab-<date>` and compare with
/// `--baseline ab-<date>`; record before, after, and absolute plus percent
/// delta per touched bench in the commit message. Timing never gates CI.
///
/// The scan topic compares the public entry points on the reference shape:
/// borrowing `parse_str` (baseline), one caller-side copy into an exactly
/// reserved buffer, and the byte entry `parse_bytes`.
fn bench_ab_scan_baseline(criterion: &mut Criterion) {
    let text = reference_fixture();
    let settings = SettingsContext::new();

    criterion.bench_function("ini_ab_scan_baseline", |bencher| {
        bencher
            .iter(|| black_box(parse_str(black_box(&text), black_box(&settings)).expect("parses")));
    });
}

fn bench_ab_scan_prereserved(criterion: &mut Criterion) {
    let text = reference_fixture();
    let settings = SettingsContext::new();

    criterion.bench_function("ini_ab_scan_prereserved", |bencher| {
        bencher.iter(|| {
            let mut owned = String::with_capacity(text.len());
            owned.push_str(&text);
            black_box(parse_str(black_box(&owned), black_box(&settings)).expect("parses"))
        });
    });
}

fn bench_ab_scan_bytes(criterion: &mut Criterion) {
    let text = reference_fixture();
    let bytes = text.into_bytes();
    let settings = SettingsContext::new();

    criterion.bench_function("ini_ab_scan_bytes", |bencher| {
        bencher.iter(|| {
            black_box(
                oxtuner_ini::parse_bytes(black_box(&bytes), black_box(&settings)).expect("parses"),
            )
        });
    });
}

/// The share topic compares cloning a parsed `Definition` per use against
/// sharing it behind `Arc`, where the public API allows both.
fn bench_ab_share_clone(criterion: &mut Criterion) {
    let text = reference_fixture();
    let settings = SettingsContext::new();
    let definition = parse_str(&text, &settings).expect("parses");

    criterion.bench_function("ini_ab_share_clone", |bencher| {
        bencher.iter(|| black_box(definition.clone()));
    });
}

fn bench_ab_share_arc(criterion: &mut Criterion) {
    let text = reference_fixture();
    let settings = SettingsContext::new();
    let definition = parse_str(&text, &settings).expect("parses");
    let shared = Arc::new(definition);

    criterion.bench_function("ini_ab_share_arc", |bencher| {
        bencher.iter(|| black_box(Arc::clone(black_box(&shared))));
    });
}

/// Local-only real-file check. The files live outside the repository and are
/// never committed; when they are absent the bench simply does not register.
/// It reports wall time and peak temporary heap against the INI.md budgets.
#[expect(
    clippy::print_stdout,
    reason = "a bench-only local report of the real-file budgets"
)]
fn bench_real_files(criterion: &mut Criterion) {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let settings = SettingsContext::new();
    let files = [
        ("speeduino", "docs/tmp/local/ini/speeduino-202501.7.ini"),
        ("rusefi", "docs/tmp/local/ini/rusefi.ini"),
        ("fome", "docs/tmp/local/ini/fome_proteus_h7.ini"),
    ];

    for (label, relative) in files {
        let path = workspace.join(relative);
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes).into_owned();

        reset_peak();
        let definition =
            parse_str(black_box(&text), black_box(&settings)).expect("real file parses");
        let peak = probe_peak_bytes();
        println!(
            "real {label}: bytes={} constants={} tables={} diagnostics={} peak={:.2} MiB",
            bytes.len(),
            definition.constants.len(),
            definition.tables.len(),
            definition.diagnostics.len(),
            f64::from(u32::try_from(peak).unwrap_or(u32::MAX)) / (1024.0 * 1024.0)
        );
        assert!(
            peak <= PEAK_LIMIT_BYTES,
            "real {label} used {peak} bytes of temporary heap, over the 30 MiB bench limit"
        );

        criterion.bench_function(&format!("ini_parse_real_{label}"), |bencher| {
            bencher.iter(|| {
                black_box(parse_str(black_box(&text), black_box(&settings)).expect("parses"))
            });
        });
        criterion.bench_function(&format!("ini_preprocess_real_{label}"), |bencher| {
            bencher.iter(|| {
                black_box(
                    preprocess(black_box(&text), black_box(&settings), None).expect("preprocesses"),
                )
            });
        });
    }
}

criterion_group!(
    benches,
    bench_preprocess,
    bench_parse_str,
    bench_parse_bytes,
    bench_parse_reference_shape,
    bench_parse_speeduino_shape,
    bench_parse_dialog_shape,
    bench_parse_config_shape,
    bench_parse_wide_tables,
    bench_cached_load,
    bench_ab_scan_baseline,
    bench_ab_scan_prereserved,
    bench_ab_scan_bytes,
    bench_ab_share_clone,
    bench_ab_share_arc,
    bench_parse_options,
    bench_preprocess_defines,
    bench_expander_build,
    bench_expand_reference,
    bench_expr,
    bench_ordered_lookup,
    bench_new_sections,
    bench_deferred_validation,
    bench_builtin_registry,
    bench_number_normalized,
    bench_repeated_sections,
    bench_include_chain,
    bench_conditional_frames,
    bench_real_files
);
criterion_main!(benches);
